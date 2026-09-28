//! Trusted, offline native-workspace import. Callers must serialize each agent's
//! startup and keep both source and namespace quiescent until this returns.
//! Only `workspaces` is employee-mounted; receipts/staging live at the agent root.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use anyhow::{anyhow, bail, ensure, Context, Result};
use rustix::fs::{openat, openat2, readlinkat, Dir, Mode, OFlags, ResolveFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::layer::LayerManager;
use crate::metadata::FileKind;

const MAX_DEPTH: usize = 64;
const MAX_NODES: u64 = 65_536;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const IO_BYTES: usize = 256 * 1024;
const SYNC_BYTES: u64 = 4 * 1024 * 1024;
const MARKER_BYTES: usize = 4096;
// Prepared receipt and its Installed replacement coexist during atomic rename.
const INITIAL_RECEIPT_RESERVATION: u64 = 2 * MARKER_BYTES as u64;
const MARKER: &str = ".sentinel-workspace-import-v1.json";
const STAGE_PREFIX: &str = ".sentinel-workspace-stage-";
const NOREPLACE: u32 = 1;

/// Outcome of importing a native tree, or recognizing an earlier import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceImportDisposition {
    Imported,
    Recovered,
    AlreadyImported,
}

/// Historical import receipt; counts/digest describe the imported tree, not
/// subsequent employee changes in an already-installed namespace. Bytes charge
/// each distinct regular inode once; nodes still include every hardlink alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceImportResult {
    pub disposition: WorkspaceImportDisposition,
    pub workspace_inode: u64,
    pub imported_nodes: u64,
    pub imported_bytes: u64,
    pub imported_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Prepared,
    Installed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    agent: String,
    source_dev: u64,
    source_ino: u64,
    stage_name: String,
    workspace_inode: u64,
    nodes: u64,
    bytes: u64,
    tree_sha256: [u8; 32],
    state: ReceiptState,
}

impl Receipt {
    fn result(&self, disposition: WorkspaceImportDisposition) -> WorkspaceImportResult {
        WorkspaceImportResult {
            disposition,
            workspace_inode: self.workspace_inode,
            imported_nodes: self.nodes,
            imported_bytes: self.bytes,
            imported_sha256: self.tree_sha256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    Regular,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    nlinks: u64,
    size: u64,
    mode: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    kind: Kind,
}

impl Identity {
    fn of(meta: &Metadata) -> Result<Self> {
        let kind = if meta.is_dir() {
            Kind::Directory
        } else if meta.is_file() {
            Kind::Regular
        } else if meta.file_type().is_symlink() {
            Kind::Symlink
        } else {
            bail!("native workspace contains an unsupported node type");
        };
        // Importing privileged mode bits is not a transfer of host authority.
        ensure!(
            meta.mode() & 0o6000 == 0,
            "privileged workspace mode is unsupported"
        );
        ensure!(
            meta.mtime() >= 0,
            "negative workspace modification time is unsupported"
        );
        ensure!(
            kind != Kind::Directory || meta.mode() & 0o111 != 0,
            "non-searchable workspace directory is unsupported"
        );
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            nlinks: meta.nlink(),
            size: meta.size(),
            mode: meta.mode() & 0o1777,
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
            kind,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeNode {
    identity: Identity,
    target: Option<String>,
}

type NativeTree = BTreeMap<String, NativeNode>;

struct CensusBudget {
    discovered: u64,
    charge: usize,
}

fn beneath() -> ResolveFlags {
    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV
}

/// Open all source ancestors without following symlinks. Mount boundaries are
/// allowed in the trusted source path, but never below the source directory.
fn open_source(path: &Path) -> Result<File> {
    ensure!(
        !path.as_os_str().is_empty(),
        "empty native workspace source"
    );
    let base = if path.is_absolute() { "/" } else { "." };
    let mut directory = File::from(openat(
        rustix::fs::CWD,
        base,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = File::from(openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?);
            }
            _ => bail!("native workspace source contains a parent/prefix component"),
        }
    }
    Ok(File::from(openat(
        &directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NOATIME | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

fn open_native(root: &File, path: &str, directory: bool) -> Result<File> {
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NOATIME | OFlags::CLOEXEC;
    if directory {
        flags |= OFlags::DIRECTORY;
    } else {
        // A concurrent replacement with a FIFO must never block trusted startup.
        flags |= OFlags::NONBLOCK;
    }
    Ok(File::from(openat2(
        root,
        path,
        flags,
        Mode::empty(),
        beneath(),
    )?))
}

fn joined(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn names(directory: &File, budget: &mut CensusBudget) -> Result<Vec<String>> {
    let fd = openat2(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOATIME | OFlags::CLOEXEC,
        Mode::empty(),
        beneath(),
    )?;
    let mut stream = Dir::new(fd)?;
    let mut names = Vec::new();
    for entry in &mut stream {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .context("non-UTF-8 workspace name")?;
        if name == "." || name == ".." {
            continue;
        }
        ensure!(
            !name.is_empty() && name.len() <= 255,
            "invalid workspace name"
        );
        // Charge discovery before recursion, including unvisited siblings. This
        // bounds the sum of name vectors retained along the traversal stack.
        ensure!(
            budget.discovered < MAX_NODES,
            "native workspace node limit exceeded"
        );
        budget.discovered += 1;
        budget.charge = budget
            .charge
            .checked_add(name.len() * 4 + 512)
            .ok_or_else(|| anyhow!("workspace index size overflow"))?;
        ensure!(
            budget.charge <= MAX_INDEX_BYTES,
            "workspace metadata index budget exceeded"
        );
        names.push(name.to_string());
    }
    names.sort();
    Ok(names)
}

fn census(root: &File) -> Result<NativeTree> {
    let identity = Identity::of(&root.metadata()?)?;
    let mut tree = BTreeMap::new();
    tree.insert(
        String::new(),
        NativeNode {
            identity,
            target: None,
        },
    );
    let mut budget = CensusBudget {
        discovered: 1,
        charge: 512,
    };
    census_directory(root, root, "", 0, &mut tree, &mut budget)?;
    let mut links: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
    for (path, node) in &tree {
        if node.identity.kind == Kind::Regular {
            let count = links
                .entry((node.identity.dev, node.identity.ino))
                .or_insert((0, node.identity.nlinks));
            ensure!(
                count.1 == node.identity.nlinks,
                "source hardlink identity changed"
            );
            count.0 += 1;
        } else if node.identity.kind == Kind::Symlink {
            ensure!(
                node.identity.nlinks == 1,
                "hardlinked symlink authority is unsupported"
            );
            validate_symlink(&tree, path, node.target.as_deref().unwrap_or_default())?;
        }
    }
    ensure!(
        links.values().all(|(inside, total)| inside == total),
        "workspace hardlink has an alias outside the source tree"
    );
    ensure!(
        native_bytes(&tree)? <= MAX_BYTES,
        "native workspace exceeds 64 MiB total logical budget"
    );
    Ok(tree)
}

fn native_bytes(tree: &NativeTree) -> Result<u64> {
    let mut inodes = HashMap::new();
    let mut bytes = 0u64;
    for node in tree
        .values()
        .filter(|node| node.identity.kind == Kind::Regular)
    {
        let identity = &node.identity;
        if let Some(previous) = inodes.insert((identity.dev, identity.ino), identity) {
            ensure!(previous == identity, "source hardlink identity changed");
        } else {
            bytes = bytes
                .checked_add(identity.size)
                .ok_or_else(|| anyhow!("workspace size overflow"))?;
        }
    }
    Ok(bytes)
}

fn census_directory(
    root: &File,
    directory: &File,
    parent: &str,
    depth: usize,
    tree: &mut NativeTree,
    budget: &mut CensusBudget,
) -> Result<()> {
    let before = Identity::of(&directory.metadata()?)?;
    for name in names(directory, budget)? {
        ensure!(depth < MAX_DEPTH, "native workspace depth limit exceeded");
        ensure!(
            tree.len() < MAX_NODES as usize,
            "native workspace node limit exceeded"
        );
        let path = joined(parent, &name);
        let inspected = File::from(openat2(
            directory,
            name.as_str(),
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            beneath(),
        )?);
        let identity = Identity::of(&inspected.metadata()?)?;
        ensure!(
            identity.dev == before.dev,
            "workspace crosses a device boundary"
        );
        let target = if identity.kind == Kind::Symlink {
            let target = readlinkat(directory, name.as_str(), Vec::new())?;
            let target = target
                .to_str()
                .context("non-UTF-8 symlink target")?
                .to_string();
            ensure!(target.len() <= 4096, "symlink target too long");
            Some(target)
        } else {
            None
        };
        budget.charge = budget
            .charge
            .checked_add(path.len() * 4 + target.as_ref().map_or(0, |t| t.len() * 4))
            .ok_or_else(|| anyhow!("workspace index size overflow"))?;
        ensure!(
            budget.charge <= MAX_INDEX_BYTES,
            "workspace metadata index budget exceeded"
        );
        tree.insert(
            path.clone(),
            NativeNode {
                identity: identity.clone(),
                target,
            },
        );
        if identity.kind == Kind::Directory {
            let child = open_native(root, &path, true)?;
            ensure!(
                Identity::of(&child.metadata()?)? == identity,
                "source directory changed before traversal"
            );
            census_directory(root, &child, &path, depth + 1, tree, budget)?;
        }
        ensure!(
            Identity::of(&inspected.metadata()?)? == identity,
            "source node changed during traversal"
        );
    }
    ensure!(
        Identity::of(&directory.metadata()?)? == before,
        "source directory changed during traversal"
    );
    Ok(())
}

fn target_components(target: &str) -> Result<VecDeque<String>> {
    ensure!(
        !target.is_empty() && !target.contains('\0'),
        "empty/invalid symlink target"
    );
    let mut components = VecDeque::new();
    for part in Path::new(target).components() {
        match part {
            Component::Normal(name) => components.push_back(
                name.to_str()
                    .context("non-UTF-8 symlink component")?
                    .to_string(),
            ),
            Component::ParentDir => components.push_back("..".to_string()),
            Component::CurDir => {}
            _ => bail!("absolute symlink leaves workspace authority"),
        }
    }
    Ok(components)
}

/// Resolve the captured graph, including intermediate symlinks before `..`.
/// Lexical normalization alone would allow escapes through symlinked parents.
fn validate_symlink(tree: &NativeTree, path: &str, target: &str) -> Result<()> {
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut stack: Vec<String> = parent
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut pending = target_components(target)?;
    let mut hops = 0;
    while let Some(component) = pending.pop_front() {
        if component == ".." {
            ensure!(stack.pop().is_some(), "symlink escapes native workspace");
            continue;
        }
        stack.push(component);
        ensure!(stack.len() <= MAX_DEPTH, "symlink depth limit exceeded");
        let relative = stack.join("/");
        let node = tree
            .get(&relative)
            .ok_or_else(|| anyhow!("unresolved workspace symlink: {path}"))?;
        if node.identity.kind == Kind::Symlink {
            hops += 1;
            ensure!(hops <= 40, "cyclic workspace symlink");
            stack.pop();
            let mut expansion = target_components(node.target.as_deref().unwrap_or_default())?;
            expansion.append(&mut pending);
            pending = expansion;
        } else if !pending.is_empty() {
            ensure!(
                node.identity.kind == Kind::Directory,
                "symlink traverses a regular file"
            );
        }
    }
    Ok(())
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn hash_entry(hash: &mut Sha256, path: &str, kind: Kind, mode: u32, mtime: u64, size: u64) {
    hash_field(hash, path.as_bytes());
    hash.update([match kind {
        Kind::Directory => 1,
        Kind::Regular => 2,
        Kind::Symlink => 3,
    }]);
    hash.update(mode.to_le_bytes());
    hash.update(mtime.to_le_bytes());
    hash.update(size.to_le_bytes());
}

fn native_digest(root: &File, tree: &NativeTree) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut aliases: HashMap<(u64, u64), &str> = HashMap::new();
    for (path, node) in tree {
        let identity = &node.identity;
        let size = if identity.kind == Kind::Directory {
            0
        } else {
            identity.size
        };
        hash_entry(
            &mut hash,
            path,
            identity.kind,
            identity.mode,
            identity.mtime as u64,
            size,
        );
        match identity.kind {
            Kind::Regular => {
                if let Some(canonical) = aliases.get(&(identity.dev, identity.ino)) {
                    hash_field(&mut hash, canonical.as_bytes());
                } else {
                    hash_field(&mut hash, &[]);
                    let mut file = open_native(root, path, false)?;
                    ensure!(
                        Identity::of(&file.metadata()?)? == *identity,
                        "source file changed before read"
                    );
                    let mut content = Sha256::new();
                    let mut bytes = 0u64;
                    let mut buffer = vec![0; IO_BYTES];
                    loop {
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        bytes += read as u64;
                        ensure!(bytes <= identity.size, "source file grew during read");
                        content.update(&buffer[..read]);
                    }
                    ensure!(
                        bytes == identity.size && Identity::of(&file.metadata()?)? == *identity,
                        "source file changed during read"
                    );
                    hash.update(content.finalize());
                    aliases.insert((identity.dev, identity.ino), path.as_str());
                }
            }
            Kind::Symlink => hash_field(
                &mut hash,
                node.target.as_deref().unwrap_or_default().as_bytes(),
            ),
            Kind::Directory => {}
        }
    }
    Ok(hash.finalize().into())
}

fn copy_tree(
    layer: &LayerManager,
    agent: &str,
    root: &File,
    tree: &NativeTree,
    stage: u64,
) -> Result<()> {
    let mut directories = HashMap::new();
    directories.insert(String::new(), stage);
    let mut aliases = HashMap::new();
    for (path, node) in tree {
        if path.is_empty() {
            continue;
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = *directories
            .get(parent)
            .ok_or_else(|| anyhow!("missing staged parent"))?;
        let identity = &node.identity;
        let inode = match identity.kind {
            Kind::Directory => {
                let inode = layer.mkdir(agent, parent, name, 0o700)?;
                directories.insert(path.clone(), inode);
                inode
            }
            Kind::Symlink => layer.symlink(
                agent,
                parent,
                name,
                node.target.as_deref().unwrap_or_default(),
            )?,
            Kind::Regular => {
                if let Some(inode) = aliases.get(&(identity.dev, identity.ino)) {
                    layer.link(agent, *inode, parent, name)?
                } else {
                    let inode = layer.create_file(agent, parent, name, 0o600, true)?;
                    let handle = layer.open_file(agent, inode, true, false, false)?;
                    let copied = (|| -> Result<()> {
                        let mut file = open_native(root, path, false)?;
                        ensure!(
                            Identity::of(&file.metadata()?)? == *identity,
                            "source file changed before copy"
                        );
                        let mut offset = 0u64;
                        let mut synced = 0u64;
                        let mut buffer = vec![0; IO_BYTES];
                        loop {
                            let read = file.read(&mut buffer)?;
                            if read == 0 {
                                break;
                            }
                            ensure!(
                                offset + read as u64 <= identity.size,
                                "source file grew during copy"
                            );
                            ensure!(
                                layer.write_handle(agent, handle, offset, &buffer[..read])? == read,
                                "short workspace write"
                            );
                            offset += read as u64;
                            // Stay below the 8 MiB/file and 64 MiB/global dirty limits.
                            if offset - synced >= SYNC_BYTES {
                                layer.sync_handle(agent, handle)?;
                                synced = offset;
                            }
                        }
                        ensure!(
                            offset == identity.size
                                && Identity::of(&file.metadata()?)? == *identity,
                            "source file changed during copy"
                        );
                        layer.sync_handle(agent, handle)?;
                        Ok(())
                    })();
                    let released = layer.release_handle(agent, handle);
                    copied?;
                    released?;
                    aliases.insert((identity.dev, identity.ino), inode);
                    inode
                }
            }
        };
        if identity.kind != Kind::Directory {
            layer.set_file_attributes(
                agent,
                inode,
                Some(identity.mode),
                None,
                None,
                None,
                Some(identity.mtime as u64),
            )?;
        }
    }
    // Children are complete before directory modes/mtime are finalized.
    for (path, node) in tree.iter().rev() {
        if node.identity.kind == Kind::Directory {
            let inode = *directories
                .get(path)
                .ok_or_else(|| anyhow!("missing staged directory"))?;
            layer.set_file_attributes(
                agent,
                inode,
                Some(node.identity.mode),
                None,
                None,
                None,
                Some(node.identity.mtime as u64),
            )?;
            layer.sync_directory(agent, inode)?;
        }
    }
    Ok(())
}

fn namespace_digest(layer: &LayerManager, agent: &str, root: u64) -> Result<([u8; 32], u64, u64)> {
    let mut index = NamespaceIndex::default();
    collect_namespace(layer, agent, root, "", 0, &mut index)?;
    let nodes = index.nodes;
    let mut links: HashMap<u64, u32> = HashMap::new();
    for (inode, data) in nodes.values() {
        if data.kind == FileKind::Regular {
            *links.entry(*inode).or_default() += 1;
        } else if data.kind == FileKind::Symlink {
            ensure!(
                data.nlinks == 1,
                "prepared namespace has an external symlink alias"
            );
        }
    }
    for (inode, data) in nodes.values() {
        if data.kind == FileKind::Regular {
            ensure!(
                links.get(inode) == Some(&data.nlinks),
                "prepared namespace has a hardlink outside its tree"
            );
        }
    }
    let mut hash = Sha256::new();
    let mut aliases: HashMap<u64, &str> = HashMap::new();
    let mut bytes = 0u64;
    for (path, (inode, data)) in &nodes {
        let kind = match data.kind {
            FileKind::Directory => Kind::Directory,
            FileKind::Regular => Kind::Regular,
            FileKind::Symlink => Kind::Symlink,
        };
        hash_entry(
            &mut hash,
            path,
            kind,
            data.mode,
            data.mtime,
            if kind == Kind::Directory {
                0
            } else {
                data.size
            },
        );
        if kind == Kind::Regular {
            if let Some(canonical) = aliases.get(inode) {
                hash_field(&mut hash, canonical.as_bytes());
            } else {
                bytes = bytes
                    .checked_add(data.size)
                    .ok_or_else(|| anyhow!("workspace size overflow"))?;
                ensure!(bytes <= MAX_BYTES, "namespace exceeds import budget");
                hash_field(&mut hash, &[]);
                let handle = layer.open_file(agent, *inode, false, false, false)?;
                let verified = (|| -> Result<[u8; 32]> {
                    let mut content = Sha256::new();
                    let mut offset = 0u64;
                    while offset < data.size {
                        let buffer = layer.read_handle(
                            agent,
                            handle,
                            offset,
                            (data.size - offset).min(IO_BYTES as u64) as usize,
                        )?;
                        ensure!(
                            !buffer.is_empty(),
                            "short namespace read during import verification"
                        );
                        offset += buffer.len() as u64;
                        ensure!(offset <= data.size, "oversized namespace read");
                        content.update(&buffer);
                    }
                    ensure!(
                        layer.read_handle(agent, handle, offset, 1)?.is_empty(),
                        "namespace grew during verification"
                    );
                    Ok(content.finalize().into())
                })();
                let released = layer.release_handle(agent, handle);
                hash.update(verified?);
                released?;
                aliases.insert(*inode, path.as_str());
            }
        } else if kind == Kind::Symlink {
            hash_field(&mut hash, data.symlink_target.as_bytes());
        }
    }
    Ok((hash.finalize().into(), nodes.len() as u64, bytes))
}

#[derive(Default)]
struct NamespaceIndex {
    nodes: BTreeMap<String, (u64, crate::metadata::InodeData)>,
    directories: HashSet<u64>,
    charge: usize,
}

fn collect_namespace(
    layer: &LayerManager,
    agent: &str,
    inode: u64,
    path: &str,
    depth: usize,
    index: &mut NamespaceIndex,
) -> Result<()> {
    ensure!(
        depth <= MAX_DEPTH && index.nodes.len() < MAX_NODES as usize,
        "namespace import verification limit exceeded"
    );
    let data = layer
        .lookup_inode(agent, inode)?
        .ok_or_else(|| anyhow!("staged inode missing"))?;
    index.charge = index
        .charge
        .checked_add(path.len() * 4 + 512 + data.symlink_target.len() * 4)
        .ok_or_else(|| anyhow!("namespace index overflow"))?;
    ensure!(
        index.charge <= MAX_INDEX_BYTES,
        "namespace index budget exceeded"
    );
    if data.kind == FileKind::Directory {
        ensure!(
            index.directories.insert(inode),
            "namespace directory alias/cycle"
        );
    }
    index.nodes.insert(path.to_string(), (inode, data.clone()));
    if data.kind == FileKind::Directory {
        let mut children = layer.readdir(agent, inode)?;
        children.sort_by(|a, b| a.0.cmp(&b.0));
        ensure!(
            children.len() <= MAX_NODES as usize,
            "namespace directory node limit exceeded"
        );
        for (name, child, _) in children {
            ensure!(
                !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
                "invalid namespace entry"
            );
            collect_namespace(layer, agent, child, &joined(path, &name), depth + 1, index)?;
        }
    }
    Ok(())
}

fn load_receipt(layer: &LayerManager, agent: &str) -> Result<Option<Receipt>> {
    let Some(inode) = layer.lookup_dirent(agent, 1, MARKER)? else {
        return Ok(None);
    };
    ensure!(
        layer.meta().get_dirent(agent, 1, MARKER)? == Some(inode),
        "import receipt must be private to the agent"
    );
    let data = layer
        .lookup_inode(agent, inode)?
        .ok_or_else(|| anyhow!("import receipt inode missing"))?;
    ensure!(
        data.kind == FileKind::Regular && data.nlinks == 1 && data.size <= MARKER_BYTES as u64,
        "invalid import receipt file"
    );
    let bytes = layer.read_file_range(agent, inode, 0, MARKER_BYTES)?;
    let receipt: Receipt =
        serde_json::from_slice(&bytes).context("invalid native workspace import receipt")?;
    ensure!(
        receipt.version == 1 && receipt.agent == agent,
        "import receipt agent/version mismatch"
    );
    ensure!(
        receipt.workspace_inode > 1
            && receipt.nodes > 0
            && receipt.nodes <= MAX_NODES
            && receipt.bytes <= MAX_BYTES,
        "invalid import receipt bounds"
    );
    ensure!(
        receipt.stage_name.starts_with(STAGE_PREFIX)
            && receipt.stage_name.len() <= 255
            && !receipt.stage_name.contains('/')
            && !receipt.stage_name.contains('\0'),
        "invalid import stage name"
    );
    Ok(Some(receipt))
}

fn store_receipt(
    layer: &LayerManager,
    agent: &str,
    receipt: &Receipt,
    replace: bool,
) -> Result<()> {
    let bytes = serde_json::to_vec(receipt)?;
    ensure!(bytes.len() <= MARKER_BYTES, "import receipt too large");
    let temporary = format!(".sentinel-workspace-receipt-{}", uuid::Uuid::new_v4());
    let inode = layer.create_file(agent, 1, &temporary, 0o600, true)?;
    let handle = layer.open_file(agent, inode, true, false, false)?;
    let stored = (|| -> Result<()> {
        ensure!(
            layer.write_handle(agent, handle, 0, &bytes)? == bytes.len(),
            "short receipt write"
        );
        layer.sync_handle(agent, handle)?;
        Ok(())
    })();
    let released = layer.release_handle(agent, handle);
    stored?;
    released?;
    layer.rename(
        agent,
        1,
        &temporary,
        1,
        MARKER,
        if replace { 0 } else { NOREPLACE },
    )?;
    layer.sync_directory(agent, 1)?;
    Ok(())
}

fn verify_receipt_tree(layer: &LayerManager, agent: &str, receipt: &Receipt) -> Result<()> {
    let (digest, nodes, bytes) = namespace_digest(layer, agent, receipt.workspace_inode)?;
    ensure!(
        digest == receipt.tree_sha256 && nodes == receipt.nodes && bytes == receipt.bytes,
        "prepared import target differs from its receipt; source and namespace preserved"
    );
    Ok(())
}

fn reserve_import_capacity(
    layer: &LayerManager,
    agent: &str,
    source_bytes: u64,
    receipt_bytes: u64,
) -> Result<()> {
    // Trusted offline caller has no open/dirty handles. Use the same aggregate
    // namespace accounting as writes, including prior stages and inherited data.
    let budget = layer.workspace_budget(agent)?;
    let required = source_bytes
        .checked_add(receipt_bytes)
        .ok_or_else(|| anyhow!("import budget overflow"))?;
    ensure!(
        budget.unlinked_live_inode_count == 0,
        "import requires a quiescent namespace without live unlinked files"
    );
    ensure!(budget.used_bytes.checked_add(required).is_some_and(|bytes| bytes <= budget.limit_bytes.min(MAX_BYTES)),
        "native workspace exceeds total namespace budget including {receipt_bytes}-byte trusted marker reservation; source and prior stages preserved");
    Ok(())
}

/// Import a trusted static native workspace before mount/spawn. Never removes or
/// writes the source. Existing unmarked destinations are explicit conflicts,
/// including empty ones. Unsupported names/nodes fail rather than being skipped.
/// Admission reserves 8 KiB for trusted receipt replacement within the existing
/// aggregate namespace limit (at most 64 MiB), not an increased employee budget.
/// An Installed receipt does not depend on the old native source surviving a
/// reboot; it binds the private destination identity and never replays content.
pub fn import_native_workspace(
    layer: &LayerManager,
    agent: &str,
    source: &Path,
) -> Result<WorkspaceImportResult> {
    ensure!(
        !agent.is_empty()
            && agent.len() <= 255
            && !agent.contains('/')
            && !agent.contains('\0')
            && agent != "."
            && agent != ".."
            && agent != crate::SHARED_BASE_LAYER_ID,
        "invalid private workspace agent"
    );
    ensure!(
        layer.artifact_plane().is_some(),
        "native import requires the shared ArtifactPlane"
    );
    layer.ensure_agent_root(agent)?;
    let destination = layer.lookup_dirent(agent, 1, "workspaces")?;
    let receipt = load_receipt(layer, agent)?;
    if let Some(receipt) = &receipt {
        if receipt.state == ReceiptState::Installed {
            ensure!(
                destination == Some(receipt.workspace_inode)
                    && layer.meta().get_dirent(agent, 1, "workspaces")?
                        == Some(receipt.workspace_inode),
                "installed workspace was removed/replaced; refusing legacy replay"
            );
            ensure!(
                layer
                    .lookup_inode(agent, receipt.workspace_inode)?
                    .is_some_and(|d| d.kind == FileKind::Directory),
                "installed workspace is not a directory"
            );
            return Ok(receipt.result(WorkspaceImportDisposition::AlreadyImported));
        }
    }
    let root =
        open_source(source).context("open static native workspace without symlink traversal")?;
    let source_identity = Identity::of(&root.metadata()?)?;
    if let Some(mut receipt) = receipt {
        ensure!(
            receipt.source_dev == source_identity.dev && receipt.source_ino == source_identity.ino,
            "import receipt source directory identity mismatch"
        );
        reserve_import_capacity(layer, agent, 0, MARKER_BYTES as u64)?;
        let tree = census(&root)?;
        ensure!(
            native_digest(&root, &tree)? == receipt.tree_sha256 && census(&root)? == tree,
            "source changed since prepared import"
        );
        ensure!(
            Identity::of(&open_source(source)?.metadata()?)? == source_identity,
            "source directory path changed during recovery"
        );
        let staged = layer.lookup_dirent(agent, 1, &receipt.stage_name)?;
        match destination {
            Some(inode) => {
                ensure!(
                    inode == receipt.workspace_inode && staged.is_none(),
                    "existing workspace conflicts with prepared import"
                );
                verify_receipt_tree(layer, agent, &receipt)?;
            }
            None => {
                ensure!(
                    staged == Some(receipt.workspace_inode),
                    "prepared import staging directory missing/replaced"
                );
                verify_receipt_tree(layer, agent, &receipt)?;
                layer.rename(agent, 1, &receipt.stage_name, 1, "workspaces", NOREPLACE)?;
            }
        }
        layer.sync_directory(agent, 1)?;
        receipt.state = ReceiptState::Installed;
        store_receipt(layer, agent, &receipt, true)?;
        return Ok(receipt.result(WorkspaceImportDisposition::Recovered));
    }
    ensure!(
        destination.is_none(),
        "existing unmarked workspaces namespace; refusing to hide accepted work"
    );
    let tree = census(&root)?;
    let bytes = native_bytes(&tree)?;
    reserve_import_capacity(layer, agent, bytes, INITIAL_RECEIPT_RESERVATION)?;
    let stage_name = format!("{STAGE_PREFIX}{}", uuid::Uuid::new_v4());
    let stage = layer.mkdir(agent, 1, &stage_name, 0o700)?;
    // Failed/partial imports deliberately retain their private stage for recovery.
    copy_tree(layer, agent, &root, &tree, stage)?;
    let digest = native_digest(&root, &tree)?;
    ensure!(
        census(&root)? == tree,
        "source tree changed while importing"
    );
    ensure!(
        Identity::of(&open_source(source)?.metadata()?)? == source_identity,
        "source directory path changed while importing"
    );
    let mut receipt = Receipt {
        version: 1,
        agent: agent.to_string(),
        source_dev: source_identity.dev,
        source_ino: source_identity.ino,
        stage_name,
        workspace_inode: stage,
        nodes: tree.len() as u64,
        bytes,
        tree_sha256: digest,
        state: ReceiptState::Prepared,
    };
    verify_receipt_tree(layer, agent, &receipt)?;
    store_receipt(layer, agent, &receipt, false)?;
    layer.rename(agent, 1, &receipt.stage_name, 1, "workspaces", NOREPLACE)?;
    layer.sync_directory(agent, 1)?;
    receipt.state = ReceiptState::Installed;
    store_receipt(layer, agent, &receipt, true)?;
    Ok(receipt.result(WorkspaceImportDisposition::Imported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_static_source_is_rejected_before_digest_adoption() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"before").unwrap();
        let root = open_source(dir.path()).unwrap();
        let tree = census(&root).unwrap();
        std::fs::write(dir.path().join("file"), b"different-sized accepted work").unwrap();
        assert!(native_digest(&root, &tree).is_err());
        assert_ne!(census(&root).unwrap(), tree);
    }

    #[test]
    fn discovery_budget_bounds_unvisited_siblings_before_recursing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one"), []).unwrap();
        std::fs::write(dir.path().join("two"), []).unwrap();
        let root = open_source(dir.path()).unwrap();
        let mut budget = CensusBudget {
            discovered: MAX_NODES - 1,
            charge: 512,
        };
        assert!(names(&root, &mut budget).is_err());
        assert_eq!(budget.discovered, MAX_NODES);
    }

    #[test]
    fn metadata_budget_rejects_discovery_before_allocating_an_unbounded_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), []).unwrap();
        let root = open_source(dir.path()).unwrap();
        let mut budget = CensusBudget {
            discovered: 1,
            charge: MAX_INDEX_BYTES,
        };
        assert!(names(&root, &mut budget).is_err());
    }
}
