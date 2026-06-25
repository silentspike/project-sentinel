//! Metadata-aware CAS manifest for a sandbox agent home (Issue #500a, Track A).
//!
//! The bwrap runtime historically snapshotted an agent home by reading every file
//! into a `BTreeMap<String, Vec<u8>>` — file *contents only*, copied as bytes, with
//! no symlink / mode / xattr / hardlink / sparse semantics. This module replaces that
//! byte copy with a **metadata-aware walk** that produces a serializable
//! [`HomeManifest`]: each entry carries full filesystem metadata plus, for regular
//! files, a list of content-defined-chunk [`BlockRef`]s (BLAKE3-128, the
//! `ArtifactPlane` chunk identity — the G2/ADR-0498 hash space) instead of bytes.
//! Rehydration reconstructs the tree with **V24 path safety**.
//!
//! Scope (honest, AC-4): this is a **format / round-trip library**. It is exercised
//! offline (walk a directory, rehydrate it elsewhere, compare). It does not touch the
//! productive daemon spawn/snapshot/migration path and claims no live migration; the
//! live integration is #548.
//!
//! ## Hash space (G2 / ADR-0498)
//! File content is chunked with the gear-hash CDC ([`crate::chunker`]) and stored in
//! the [`ArtifactPlane`] (BLAKE3-128 chunk identity). Each chunk is referenced by a
//! `BlockRef { namespace: Chunk, algorithm: Blake3_128, .. }`. A whole-content SHA-256
//! ([`ManifestEntry::object_sha256`]) is kept per file as the integrity/compliance hash
//! (it is *not* the dedup key). No second SHA-256 chunk store is created.
//!
//! ## Chunk lifecycle (N1')
//! `commit_ingest` increments `FS_CHUNK_REFCOUNT` for every chunk, so the chunks of a
//! freshly walked home survive a GC scan as long as their ingest objects exist. The
//! object ids are returned in [`WalkOutput::owned_object_ids`] (kept out of the
//! serializable manifest so the snapshot payload stays deterministic, N5) and reaped
//! via [`release_manifest`]; that drops the refcounts and makes the chunks GC-able.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rustix::fs::{
    fchmod, linkat, mkdirat, openat2, symlinkat, AtFlags, Mode, OFlags, ResolveFlags,
};
use sentinel_common::{BlockRef, HashAlgorithm};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{ArtifactPlane, ChunkHash};
use crate::chunker::chunk_data;
use crate::ingest::{begin_ingest, commit_ingest};

/// Chunk boundary profile recorded in every chunk `BlockRef`. Matches the
/// [`crate::chunker`] defaults (16K / 64K / 256K, gear-hash CDC).
pub const CHUNK_PROFILE: &str = "gear-v1:16k-64k-256k";

/// MIME hint used when ingesting raw file content into the chunk store.
const CONTENT_MIME: &str = "application/octet-stream";

/// Manifest format version.
const MANIFEST_VERSION: u16 = 1;

/// The kind of a filesystem entry in a [`ManifestEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A regular file (content captured as chunk refs).
    File,
    /// A directory.
    Dir,
    /// A symbolic link (target captured, no content).
    Symlink,
    /// A second name for a regular file already captured (inode dedup).
    Hardlink,
    /// A named pipe (FIFO).
    Fifo,
    /// A unix-domain socket node.
    Socket,
    /// A character device node.
    CharDevice,
    /// A block device node.
    BlockDevice,
}

/// uid/gid policy for an entry (V24): the raw host ids are recorded for
/// observability only and are **never** restored; rehydration maps to the
/// sandbox identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdPolicy {
    /// Map to the sandbox identity on restore; the observed raw host ids are
    /// retained only for auditing.
    SandboxMapped {
        /// The uid observed on the host during the walk (not restored).
        raw_uid_observed: u32,
        /// The gid observed on the host during the walk (not restored).
        raw_gid_observed: u32,
    },
}

/// Modification-time policy for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MtimePolicy {
    /// Preserve the recorded mtime (nanoseconds since the unix epoch). Fixed
    /// granularity so a re-walk reads back an identical value (N5: keeps the
    /// snapshot payload deterministic for the conformance harness).
    Preserve(u64),
    /// Reset the mtime to "now" on restore.
    Reset,
}

/// One contiguous run of real data in a file's content. Holes between extents
/// are never read or chunked, so the chunk refs contain no zero-fill bytes.
/// A dense file is a single extent at offset 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataExtent {
    /// Byte offset of this data run within the file.
    pub offset: u64,
    /// Ordered content-defined chunks (BLAKE3-128) covering this run.
    pub chunk_refs: Vec<BlockRef>,
}

/// A single filesystem entry in a [`HomeManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the home root, as raw bytes (non-UTF-8 safe).
    pub rel_path_bytes: Vec<u8>,
    /// The entry kind.
    pub kind: EntryKind,
    /// The permission bits (`st_mode & 0o7777`).
    pub mode: u32,
    /// uid policy (V24).
    pub uid_policy: IdPolicy,
    /// gid policy (V24).
    pub gid_policy: IdPolicy,
    /// mtime policy.
    pub mtime_policy: MtimePolicy,
    /// Extended attributes, sorted by name for determinism.
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    /// Symlink target (raw bytes), present only for [`EntryKind::Symlink`].
    pub symlink_target: Option<Vec<u8>>,
    /// For [`EntryKind::Hardlink`], the rel-path of the canonical entry.
    pub hardlink_to: Option<Vec<u8>>,
    /// Device id (`st_rdev`) for device nodes.
    pub rdev: u64,
    /// Logical file size in bytes.
    pub size: u64,
    /// SHA-256 over the full logical content (holes as zero) — integrity hash
    /// for [`EntryKind::File`]. `None` for non-files.
    pub object_sha256: Option<[u8; 32]>,
    /// File content as data extents (empty for non-files).
    pub content: Vec<DataExtent>,
}

/// A serializable, metadata-aware CAS manifest of a sandbox home.
///
/// This is the wire format: it is content-addressed (chunks referenced by hash)
/// and fully deterministic (entries sorted by path, xattrs sorted by name), so a
/// re-walk of an identical tree produces an identical manifest. It deliberately
/// carries no `ArtifactPlane` object ids (those are non-deterministic local
/// bookkeeping — see [`WalkOutput::owned_object_ids`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeManifest {
    /// Manifest format version.
    pub version: u16,
    /// The chunk boundary profile used for all chunk refs.
    pub chunk_profile: String,
    /// Entries, sorted by `rel_path_bytes`.
    pub entries: Vec<ManifestEntry>,
}

/// Result of [`walk_home`]: the wire manifest plus the local `ArtifactPlane`
/// object ids that pin its chunks (for [`release_manifest`]).
#[derive(Debug, Clone)]
pub struct WalkOutput {
    /// The serializable manifest.
    pub manifest: HomeManifest,
    /// `ArtifactPlane` object ids created by this walk; pass to
    /// [`release_manifest`] to unpin the chunks when the snapshot is dropped.
    pub owned_object_ids: Vec<u64>,
}

/// Restore-side policy (V24). The default maps to sandbox uid/gid 0, forbids
/// device nodes, and does not chown (so it works unprivileged).
#[derive(Debug, Clone, Default)]
pub struct RestorePolicy {
    /// The sandbox uid files are owned by after restore (never the raw host uid).
    pub sandbox_uid: u32,
    /// The sandbox gid files are owned by after restore.
    pub sandbox_gid: u32,
    /// Whether device/fifo/socket nodes may be recreated (default: false).
    pub allow_devices: bool,
    /// Whether to chown to the sandbox identity (needs privilege; default: false).
    pub apply_chown: bool,
}

// ───────────────────────── Path safety (V24, Finding D) ─────────────────────────

/// Lexically validate a relative path from a manifest (no filesystem access, so
/// no TOCTOU and no symlink following). Rejects absolute paths and any `.` or
/// `..` component. Returns the cleaned [`PathBuf`].
pub fn validate_rel_path(rel: &[u8]) -> Result<PathBuf> {
    if rel.is_empty() {
        bail!("manifest entry has empty path");
    }
    let path = Path::new(OsStr::from_bytes(rel));
    for comp in path.components() {
        match comp {
            Component::Normal(_) => {}
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute path in manifest entry: {:?}", path)
            }
            Component::ParentDir => bail!("'..' component in manifest entry: {:?}", path),
            Component::CurDir => bail!("'.' component in manifest entry: {:?}", path),
        }
    }
    Ok(path.to_path_buf())
}

/// Lexically validate a symlink target relative to the dest root. Rejects an
/// absolute target and any target that would escape the root via `..`. This is
/// an early defense; the write-time `openat2` resolution (Finding E) is the
/// authoritative one.
fn validate_symlink_target(target: &[u8]) -> Result<()> {
    let path = Path::new(OsStr::from_bytes(target));
    if path.is_absolute() {
        bail!("absolute symlink target: {:?}", path);
    }
    let mut depth: i64 = 0;
    for comp in path.components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    bail!("symlink target escapes root via '..': {:?}", path);
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute symlink target: {:?}", path)
            }
        }
    }
    Ok(())
}

// ───────────────────────────────── Walk ────────────────────────────────────────

/// Walk a home directory and produce a metadata-aware CAS manifest. File content
/// is chunked into the `plane` (no bytes in the manifest); the returned
/// [`WalkOutput::owned_object_ids`] pin those chunks.
pub fn walk_home(root: &Path, plane: &ArtifactPlane) -> Result<WalkOutput> {
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut owned_object_ids: Vec<u64> = Vec::new();
    // (st_dev, st_ino) -> first rel-path seen, for hardlink dedup.
    let mut inode_seen: HashMap<(u64, u64), Vec<u8>> = HashMap::new();

    if root.exists() {
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let mut children: Vec<PathBuf> = std::fs::read_dir(&dir)
                .with_context(|| format!("read_dir {}", dir.display()))?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<_>>()?;
            children.sort();
            for path in children {
                let meta = std::fs::symlink_metadata(&path)
                    .with_context(|| format!("lstat {}", path.display()))?;
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .as_os_str()
                    .as_bytes()
                    .to_vec();
                let ft = meta.file_type();

                let entry = if ft.is_dir() {
                    stack.push(path.clone());
                    base_entry(rel, EntryKind::Dir, &meta, &path)
                } else if ft.is_symlink() {
                    let target = std::fs::read_link(&path)
                        .with_context(|| format!("readlink {}", path.display()))?;
                    let mut e = base_entry(rel, EntryKind::Symlink, &meta, &path);
                    e.symlink_target = Some(target.as_os_str().as_bytes().to_vec());
                    e
                } else if ft.is_file() {
                    // Hardlink dedup: a second name for an inode we already captured.
                    if meta.nlink() > 1 {
                        let key = (meta.dev(), meta.ino());
                        if let Some(canonical) = inode_seen.get(&key) {
                            let mut e = base_entry(rel, EntryKind::Hardlink, &meta, &path);
                            e.hardlink_to = Some(canonical.clone());
                            entries.push(e);
                            continue;
                        }
                        inode_seen.insert(key, rel.clone());
                    }
                    let mut e = base_entry(rel, EntryKind::File, &meta, &path);
                    capture_file_content(&path, &meta, plane, &mut e, &mut owned_object_ids)?;
                    e
                } else if ft.is_fifo() {
                    base_entry(rel, EntryKind::Fifo, &meta, &path)
                } else if ft.is_socket() {
                    base_entry(rel, EntryKind::Socket, &meta, &path)
                } else if ft.is_char_device() {
                    base_entry(rel, EntryKind::CharDevice, &meta, &path)
                } else if ft.is_block_device() {
                    base_entry(rel, EntryKind::BlockDevice, &meta, &path)
                } else {
                    bail!("unsupported file type at {}", path.display());
                };
                entries.push(entry);
            }
        }
    }

    entries.sort_by(|a, b| a.rel_path_bytes.cmp(&b.rel_path_bytes));

    Ok(WalkOutput {
        manifest: HomeManifest {
            version: MANIFEST_VERSION,
            chunk_profile: CHUNK_PROFILE.to_string(),
            entries,
        },
        owned_object_ids,
    })
}

/// Build the common metadata fields shared by every entry kind.
fn base_entry(
    rel: Vec<u8>,
    kind: EntryKind,
    meta: &std::fs::Metadata,
    path: &Path,
) -> ManifestEntry {
    let mtime_ns = (meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128).max(0) as u64;
    ManifestEntry {
        rel_path_bytes: rel,
        kind,
        mode: meta.permissions().mode() & 0o7777,
        uid_policy: IdPolicy::SandboxMapped {
            raw_uid_observed: meta.uid(),
            raw_gid_observed: meta.gid(),
        },
        gid_policy: IdPolicy::SandboxMapped {
            raw_uid_observed: meta.uid(),
            raw_gid_observed: meta.gid(),
        },
        mtime_policy: MtimePolicy::Preserve(mtime_ns),
        xattrs: read_xattrs(path),
        symlink_target: None,
        hardlink_to: None,
        rdev: meta.rdev(),
        size: meta.size(),
        object_sha256: None,
        content: Vec::new(),
    }
}

/// Capture a regular file's content as data extents of chunk refs, ingesting the
/// chunks into the `ArtifactPlane`.
fn capture_file_content(
    path: &Path,
    meta: &std::fs::Metadata,
    plane: &ArtifactPlane,
    entry: &mut ManifestEntry,
    owned_object_ids: &mut Vec<u64>,
) -> Result<()> {
    let size = meta.size();
    // Sparse iff fewer blocks are allocated than a dense file would need.
    let sparse = size > 0 && (meta.blocks() * 512) < size;

    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;

    // object_sha256 always covers the full logical content (holes as zero).
    let mut full = Vec::with_capacity(size as usize);
    file.read_to_end(&mut full)
        .with_context(|| format!("read {}", path.display()))?;
    entry.object_sha256 = Some(Sha256::digest(&full).into());

    let runs: Vec<(u64, u64)> = if sparse {
        data_extents(&file, size)?
    } else {
        vec![(0, size)]
    };

    let mut content = Vec::with_capacity(runs.len());
    for (offset, len) in runs {
        if len == 0 {
            continue;
        }
        let slice = &full[offset as usize..(offset + len) as usize];
        let chunk_refs = ingest_run(slice, plane, owned_object_ids)?;
        content.push(DataExtent { offset, chunk_refs });
    }
    entry.content = content;
    Ok(())
}

/// Ingest one data run, returning its ordered chunk refs (BLAKE3-128).
fn ingest_run(
    data: &[u8],
    plane: &ArtifactPlane,
    owned_object_ids: &mut Vec<u64>,
) -> Result<Vec<BlockRef>> {
    // Chunk locally to obtain per-chunk hash + length for the BlockRefs...
    let chunks: Vec<_> = chunk_data(data).collect();
    // ...and ingest the same bytes for storage (deduped, refcounted).
    let mut session = begin_ingest(plane, CONTENT_MIME);
    session.write(data);
    let object_id = commit_ingest(session)?;
    owned_object_ids.push(object_id);

    Ok(chunks
        .into_iter()
        .map(|c| BlockRef::chunk_blake3_128(c.hash, c.data.len() as u64, CHUNK_PROFILE))
        .collect())
}

// ───────────────────────────────── Rehydrate ───────────────────────────────────

/// Rehydrate a manifest into `dest` from the `ArtifactPlane`, applying V24 path
/// safety. Two passes: regular tree first (dirs, files, symlinks, special),
/// hardlinks second (so their canonical targets already exist — Finding A).
pub fn rehydrate(
    manifest: &HomeManifest,
    dest: &Path,
    plane: &ArtifactPlane,
    policy: &RestorePolicy,
) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("create dest {}", dest.display()))?;
    let dest_fd = rustix::fs::open(
        dest,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("open dest dir {}", dest.display()))?;
    let dest_fd = dest_fd.as_fd();

    // Pass A: everything except hardlinks (entries are already path-sorted, so
    // parent dirs precede their children).
    for entry in &manifest.entries {
        if entry.kind == EntryKind::Hardlink {
            continue;
        }
        let rel = validate_rel_path(&entry.rel_path_bytes)?;
        match entry.kind {
            EntryKind::Dir => create_dir(dest_fd, &rel, entry, policy)?,
            EntryKind::File => create_file(dest_fd, &rel, entry, plane, policy)?,
            EntryKind::Symlink => create_symlink(dest_fd, &rel, entry)?,
            EntryKind::Fifo
            | EntryKind::Socket
            | EntryKind::CharDevice
            | EntryKind::BlockDevice => {
                if !policy.allow_devices {
                    bail!(
                        "refusing to restore device/special node {:?} (allow_devices=false)",
                        rel
                    );
                }
                create_special(dest_fd, &rel, entry)?;
            }
            EntryKind::Hardlink => unreachable!(),
        }
    }

    // Pass B: hardlinks.
    for entry in &manifest.entries {
        if entry.kind != EntryKind::Hardlink {
            continue;
        }
        let rel = validate_rel_path(&entry.rel_path_bytes)?;
        let target_bytes = entry
            .hardlink_to
            .as_ref()
            .ok_or_else(|| anyhow!("hardlink entry {:?} without target", rel))?;
        let target = validate_rel_path(target_bytes)?;
        create_hardlink(dest_fd, &rel, &target)?;
    }

    Ok(())
}

/// Resolve the parent directory of `rel` to an `O_PATH` fd, refusing to traverse
/// any symlink and refusing to escape `dest_fd` (Finding E). The leaf name is
/// returned separately.
fn open_parent<'a>(dest_fd: BorrowedFd<'_>, rel: &'a Path) -> Result<(OwnedFd, &'a OsStr)> {
    let leaf = rel
        .file_name()
        .ok_or_else(|| anyhow!("path has no final component: {:?}", rel))?;
    let parent = rel.parent().filter(|p| !p.as_os_str().is_empty());
    let parent_path: &Path = parent.unwrap_or_else(|| Path::new("."));
    let fd = openat2(
        dest_fd,
        parent_path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )
    .with_context(|| format!("openat2 parent {:?} (symlink/escape rejected)", parent_path))?;
    Ok((fd, leaf))
}

fn create_dir(
    dest_fd: BorrowedFd<'_>,
    rel: &Path,
    entry: &ManifestEntry,
    policy: &RestorePolicy,
) -> Result<()> {
    let (parent, leaf) = open_parent(dest_fd, rel)?;
    mkdirat(&parent, leaf, Mode::from_raw_mode(entry.mode))
        .with_context(|| format!("mkdirat {:?}", rel))?;
    // Re-open the created dir (no-follow) to set mode/owner/xattr precisely.
    let dir_fd = openat2(
        &parent,
        leaf,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )
    .with_context(|| format!("reopen dir {:?}", rel))?;
    fchmod(&dir_fd, Mode::from_raw_mode(entry.mode))?;
    apply_owner(&dir_fd, policy)?;
    set_xattrs(&dir_fd, &entry.xattrs)?;
    Ok(())
}

fn create_file(
    dest_fd: BorrowedFd<'_>,
    rel: &Path,
    entry: &ManifestEntry,
    plane: &ArtifactPlane,
    policy: &RestorePolicy,
) -> Result<()> {
    let (parent, leaf) = open_parent(dest_fd, rel)?;
    let fd = openat2(
        &parent,
        leaf,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(entry.mode),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )
    .with_context(|| format!("create file {:?}", rel))?;
    let mut file = File::from(fd);

    // Pre-size so holes between data extents stay holes.
    rustix::fs::ftruncate(&file, entry.size)?;
    for ext in &entry.content {
        let hashes: Vec<ChunkHash> = ext
            .chunk_refs
            .iter()
            .map(chunk_hash_of)
            .collect::<Result<_>>()?;
        let blocks = plane.read_chunks_decompressed(&hashes)?;
        let mut at = ext.offset;
        for block in blocks {
            rustix::fs::seek(&file, rustix::fs::SeekFrom::Start(at))?;
            file.write_all(&block)?;
            at += block.len() as u64;
        }
    }

    fchmod(&file, Mode::from_raw_mode(entry.mode))?;
    apply_owner(&file, policy)?;
    set_xattrs(&file, &entry.xattrs)?;
    apply_mtime(&file, &entry.mtime_policy)?;
    Ok(())
}

fn create_symlink(dest_fd: BorrowedFd<'_>, rel: &Path, entry: &ManifestEntry) -> Result<()> {
    let target = entry
        .symlink_target
        .as_ref()
        .ok_or_else(|| anyhow!("symlink entry {:?} without target", rel))?;
    validate_symlink_target(target)?;
    let (parent, leaf) = open_parent(dest_fd, rel)?;
    symlinkat(OsStr::from_bytes(target), &parent, leaf)
        .with_context(|| format!("symlinkat {:?}", rel))?;
    Ok(())
}

fn create_special(dest_fd: BorrowedFd<'_>, rel: &Path, entry: &ManifestEntry) -> Result<()> {
    let (parent, leaf) = open_parent(dest_fd, rel)?;
    let file_type = match entry.kind {
        EntryKind::Fifo => rustix::fs::FileType::Fifo,
        EntryKind::Socket => rustix::fs::FileType::Socket,
        EntryKind::CharDevice => rustix::fs::FileType::CharacterDevice,
        EntryKind::BlockDevice => rustix::fs::FileType::BlockDevice,
        _ => bail!("create_special on non-special kind"),
    };
    let dev = if matches!(entry.kind, EntryKind::CharDevice | EntryKind::BlockDevice) {
        entry.rdev
    } else {
        0
    };
    rustix::fs::mknodat(
        &parent,
        leaf,
        file_type,
        Mode::from_raw_mode(entry.mode),
        dev,
    )
    .with_context(|| format!("mknodat {:?}", rel))?;
    Ok(())
}

fn create_hardlink(dest_fd: BorrowedFd<'_>, rel: &Path, target: &Path) -> Result<()> {
    let (link_parent, link_leaf) = open_parent(dest_fd, rel)?;
    let (target_parent, target_leaf) = open_parent(dest_fd, target)?;
    linkat(
        &target_parent,
        target_leaf,
        &link_parent,
        link_leaf,
        AtFlags::empty(),
    )
    .with_context(|| format!("linkat {:?} -> {:?}", rel, target))?;
    Ok(())
}

/// V24: never chown to the raw host uid/gid; chown to the sandbox identity only
/// when the policy asks for it (and we have the privilege to).
fn apply_owner<Fd: AsFd>(fd: Fd, policy: &RestorePolicy) -> Result<()> {
    if !policy.apply_chown {
        return Ok(());
    }
    let uid = rustix::fs::Uid::from_raw(policy.sandbox_uid);
    let gid = rustix::fs::Gid::from_raw(policy.sandbox_gid);
    rustix::fs::fchown(fd, Some(uid), Some(gid)).context("fchown to sandbox identity")?;
    Ok(())
}

fn apply_mtime<Fd: AsFd>(fd: Fd, policy: &MtimePolicy) -> Result<()> {
    if let MtimePolicy::Preserve(ns) = policy {
        let ts = rustix::fs::Timespec {
            tv_sec: (*ns / 1_000_000_000) as i64,
            tv_nsec: (*ns % 1_000_000_000) as _,
        };
        let times = rustix::fs::Timestamps {
            last_access: ts,
            last_modification: ts,
        };
        rustix::fs::futimens(fd, &times).context("futimens preserve mtime")?;
    }
    Ok(())
}

/// Convert a chunk `BlockRef` back to a 16-byte [`ChunkHash`].
fn chunk_hash_of(r: &BlockRef) -> Result<ChunkHash> {
    if r.algorithm() != HashAlgorithm::Blake3_128 {
        bail!("content block ref is not blake3-128: {:?}", r.algorithm());
    }
    r.digest()
        .try_into()
        .map_err(|_| anyhow!("blake3-128 digest is not 16 bytes"))
}

// ───────────────────────────────── Lifecycle (N1') ─────────────────────────────

/// Release the `ArtifactPlane` objects created by a walk, decrementing the chunk
/// refcounts so the chunks become GC-able (idempotent).
pub fn release_manifest(plane: &ArtifactPlane, owned_object_ids: &[u64]) -> Result<()> {
    for &id in owned_object_ids {
        crate::gc::release_object(plane, id)?;
    }
    Ok(())
}

// ───────────────────────────────── xattr / sparse FFI (Tier-2) ──────────────────

/// Read all extended attributes of `path` (no-follow), sorted by name. Returns
/// an empty list if the filesystem does not support xattrs.
fn read_xattrs(path: &Path) -> Vec<(Vec<u8>, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    // First call sizes the name list.
    let size = unsafe { libc::llistxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if size <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    let got = unsafe { libc::llistxattr(c_path.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    if got <= 0 {
        return Vec::new();
    }
    buf.truncate(got as usize);

    let mut out = Vec::new();
    for name in buf.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let c_name = match std::ffi::CString::new(name) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let vsize =
            unsafe { libc::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
        if vsize < 0 {
            continue;
        }
        let mut val = vec![0u8; vsize as usize];
        let vgot = unsafe {
            libc::lgetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                val.as_mut_ptr() as *mut _,
                val.len(),
            )
        };
        if vgot < 0 {
            continue;
        }
        val.truncate(vgot as usize);
        out.push((name.to_vec(), val));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Set extended attributes on an open fd (best-effort; errors are surfaced only
/// when there are xattrs to set, so unsupported filesystems do not break Tier-1).
fn set_xattrs<Fd: AsFd>(fd: Fd, xattrs: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    if xattrs.is_empty() {
        return Ok(());
    }
    let raw = fd.as_fd().as_raw_fd();
    for (name, value) in xattrs {
        let c_name =
            std::ffi::CString::new(name.clone()).map_err(|_| anyhow!("xattr name has NUL"))?;
        let rc = unsafe {
            libc::fsetxattr(
                raw,
                c_name.as_ptr(),
                value.as_ptr() as *const _,
                value.len(),
                0,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("fsetxattr {:?}", String::from_utf8_lossy(name)));
        }
    }
    Ok(())
}

/// Enumerate the data extents of a sparse file via `SEEK_DATA` / `SEEK_HOLE`.
fn data_extents(file: &File, size: u64) -> Result<Vec<(u64, u64)>> {
    use std::os::fd::AsRawFd as _;
    let fd = file.as_raw_fd();
    let mut extents = Vec::new();
    let mut pos: i64 = 0;
    let end = size as i64;
    while pos < end {
        let data = unsafe { libc::lseek(fd, pos, libc::SEEK_DATA) };
        if data < 0 {
            // ENXIO == no more data between pos and EOF.
            break;
        }
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        if hole < 0 {
            bail!("SEEK_HOLE failed: {}", std::io::Error::last_os_error());
        }
        let hole = hole.min(end);
        if hole > data {
            extents.push((data as u64, (hole - data) as u64));
        }
        pos = hole;
    }
    Ok(extents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("hm.redb")).unwrap();
        (plane, dir)
    }

    // AC-1: the manifest is metadata-aware (block refs + metadata), not bytes.
    #[test]
    fn ac1_manifest_holds_block_refs_not_bytes() {
        let (plane, _pd) = temp_plane();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("hello.txt"), b"hello world").unwrap();
        let out = walk_home(home.path(), &plane).unwrap();
        let file = out
            .manifest
            .entries
            .iter()
            .find(|e| e.rel_path_bytes == b"hello.txt")
            .unwrap();
        assert_eq!(file.kind, EntryKind::File);
        assert!(file.object_sha256.is_some());
        assert!(!file.content.is_empty());
        let refs = &file.content[0].chunk_refs;
        assert!(!refs.is_empty());
        assert_eq!(refs[0].algorithm(), HashAlgorithm::Blake3_128);
    }

    // AC-2: byte- and metadata-identical round-trip, including a hardlink whose
    // canonical path sorts AFTER the link (Finding A), and mode preservation.
    #[test]
    fn ac2_roundtrip_files_symlink_hardlink_mode() {
        let (plane, _pd) = temp_plane();
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::create_dir(h.join("sub")).unwrap();
        std::fs::write(h.join("sub/z.txt"), b"canonical content for hardlink").unwrap();
        std::fs::create_dir(h.join("a_dir")).unwrap();
        // Finding A: the canonical regular file is top-level "z_canon.bin" (seen
        // first by the DFS), and the hardlink is "a_dir/link.bin" — whose path
        // sorts BEFORE "z_canon.bin". A single sorted pass would try to link
        // before the target exists; only the two-pass restore succeeds.
        std::fs::write(h.join("z_canon.bin"), vec![7u8; 200_000]).unwrap();
        std::fs::hard_link(h.join("z_canon.bin"), h.join("a_dir/link.bin")).unwrap();
        std::os::unix::fs::symlink("sub/z.txt", h.join("link_to_z")).unwrap();
        // a file with explicit mode bits
        let scriptp = h.join("run.sh");
        std::fs::write(&scriptp, b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&scriptp, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = walk_home(h, &plane).unwrap();
        // Confirm the hardlink entry really sorts before its canonical target.
        let pos_link = out
            .manifest
            .entries
            .iter()
            .position(|e| e.rel_path_bytes == b"a_dir/link.bin")
            .unwrap();
        let pos_canon = out
            .manifest
            .entries
            .iter()
            .position(|e| e.rel_path_bytes == b"z_canon.bin")
            .unwrap();
        assert!(
            pos_link < pos_canon,
            "Finding A: link sorts before canonical"
        );
        assert_eq!(out.manifest.entries[pos_link].kind, EntryKind::Hardlink);
        assert_eq!(out.manifest.entries[pos_canon].kind, EntryKind::File);

        let dest = tempfile::tempdir().unwrap();
        rehydrate(
            &out.manifest,
            dest.path(),
            &plane,
            &RestorePolicy::default(),
        )
        .unwrap();

        // content sha256 equal
        assert_eq!(
            sha256_file(&h.join("z_canon.bin")),
            sha256_file(&dest.path().join("z_canon.bin")),
            "regular file content must match"
        );
        assert_eq!(
            std::fs::read(h.join("sub/z.txt")).unwrap(),
            std::fs::read(dest.path().join("sub/z.txt")).unwrap()
        );
        // hardlink: same inode as its canonical
        let ino_canon = std::fs::metadata(dest.path().join("z_canon.bin"))
            .unwrap()
            .ino();
        let ino_link = std::fs::metadata(dest.path().join("a_dir/link.bin"))
            .unwrap()
            .ino();
        assert_eq!(ino_canon, ino_link, "hardlink must share the inode");
        // symlink target preserved
        let tgt = std::fs::read_link(dest.path().join("link_to_z")).unwrap();
        assert_eq!(tgt, Path::new("sub/z.txt"));
        // mode preserved
        let mode = std::fs::symlink_metadata(dest.path().join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o755, "executable mode must be preserved");
    }

    // AC-3: V24 path-safety negatives.
    #[test]
    fn ac3_path_safety_negatives() {
        let (plane, _pd) = temp_plane();
        let dest = tempfile::tempdir().unwrap();

        // (a) absolute path
        assert!(validate_rel_path(b"/etc/passwd").is_err());
        // (b) parent traversal
        assert!(validate_rel_path(b"../escape").is_err());
        assert!(validate_rel_path(b"a/../../escape").is_err());
        // symlink target escape
        assert!(validate_symlink_target(b"/etc").is_err());
        assert!(validate_symlink_target(b"../../etc").is_err());

        // (d) write-through-symlink: an in-dest symlink "link" -> "/tmp", then a
        // file entry "link/passwd" must be REJECTED by openat2 NO_SYMLINKS.
        let manifest = HomeManifest {
            version: MANIFEST_VERSION,
            chunk_profile: CHUNK_PROFILE.to_string(),
            entries: vec![
                ManifestEntry {
                    rel_path_bytes: b"link".to_vec(),
                    kind: EntryKind::Symlink,
                    mode: 0o777,
                    uid_policy: IdPolicy::SandboxMapped {
                        raw_uid_observed: 0,
                        raw_gid_observed: 0,
                    },
                    gid_policy: IdPolicy::SandboxMapped {
                        raw_uid_observed: 0,
                        raw_gid_observed: 0,
                    },
                    mtime_policy: MtimePolicy::Reset,
                    xattrs: vec![],
                    symlink_target: Some(b"sub".to_vec()),
                    hardlink_to: None,
                    rdev: 0,
                    size: 0,
                    object_sha256: None,
                    content: vec![],
                },
                ManifestEntry {
                    rel_path_bytes: b"link/passwd".to_vec(),
                    kind: EntryKind::File,
                    mode: 0o644,
                    uid_policy: IdPolicy::SandboxMapped {
                        raw_uid_observed: 0,
                        raw_gid_observed: 0,
                    },
                    gid_policy: IdPolicy::SandboxMapped {
                        raw_uid_observed: 0,
                        raw_gid_observed: 0,
                    },
                    mtime_policy: MtimePolicy::Reset,
                    xattrs: vec![],
                    symlink_target: None,
                    hardlink_to: None,
                    rdev: 0,
                    size: 0,
                    object_sha256: Some([0u8; 32]),
                    content: vec![],
                },
            ],
        };
        // "sub" does not exist so the symlink dangles; the point is the file under
        // it must be rejected, never written through the symlink.
        let err = rehydrate(&manifest, dest.path(), &plane, &RestorePolicy::default());
        assert!(err.is_err(), "write-through-symlink must be rejected");
        assert!(!dest.path().join("sub").join("passwd").exists());

        // (e) device node without allow_devices
        let dev_manifest = HomeManifest {
            version: MANIFEST_VERSION,
            chunk_profile: CHUNK_PROFILE.to_string(),
            entries: vec![ManifestEntry {
                rel_path_bytes: b"null".to_vec(),
                kind: EntryKind::CharDevice,
                mode: 0o666,
                uid_policy: IdPolicy::SandboxMapped {
                    raw_uid_observed: 0,
                    raw_gid_observed: 0,
                },
                gid_policy: IdPolicy::SandboxMapped {
                    raw_uid_observed: 0,
                    raw_gid_observed: 0,
                },
                mtime_policy: MtimePolicy::Reset,
                xattrs: vec![],
                symlink_target: None,
                hardlink_to: None,
                rdev: 0x103,
                size: 0,
                object_sha256: None,
                content: vec![],
            }],
        };
        let dest2 = tempfile::tempdir().unwrap();
        assert!(rehydrate(
            &dev_manifest,
            dest2.path(),
            &plane,
            &RestorePolicy::default()
        )
        .is_err());
    }

    // AC-8: chunk lifecycle — manifest chunks survive a GC scan; release makes
    // them GC-able; no untracked transient object remains.
    #[test]
    fn ac8_chunk_lifecycle() {
        let (plane, _pd) = temp_plane();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("big.bin"), vec![0xACu8; 300_000]).unwrap();
        let out = walk_home(home.path(), &plane).unwrap();
        assert!(!out.owned_object_ids.is_empty());

        // every chunk is referenced (refcount > 0)
        let file = &out.manifest.entries[0];
        let hashes: Vec<ChunkHash> = file.content[0]
            .chunk_refs
            .iter()
            .map(|r| chunk_hash_of(r).unwrap())
            .collect();
        for h in &hashes {
            assert!(plane.get_chunk_refcount(h).unwrap() > 0);
        }
        // a GC scan finds no orphans -> chunks survive
        assert!(crate::gc::gc_chunks(&plane).unwrap().trashed == 0);

        // release -> refcounts drop -> chunks become orphans
        release_manifest(&plane, &out.owned_object_ids).unwrap();
        for h in &hashes {
            assert_eq!(plane.get_chunk_refcount(h).unwrap(), 0);
        }
        assert!(crate::gc::gc_chunks(&plane).unwrap().trashed > 0);
        // no transient object remains
        for &id in &out.owned_object_ids {
            assert!(plane.get_object(id).unwrap().is_none());
        }
    }

    fn sha256_file(p: &Path) -> [u8; 32] {
        Sha256::digest(std::fs::read(p).unwrap()).into()
    }

    // ── Tier-2 (VM ext4): xattr + sparse need real filesystem support ──

    #[test]
    #[ignore = "needs a filesystem with user-xattr support (VM ext4, not tmpfs)"]
    fn tier2_xattr_roundtrip() {
        let (plane, _pd) = temp_plane();
        let home = tempfile::tempdir().unwrap();
        let fp = home.path().join("withx.txt");
        std::fs::write(&fp, b"data").unwrap();
        set_test_xattr(&fp, b"user.sentinel", b"v500a");

        let out = walk_home(home.path(), &plane).unwrap();
        let e = out
            .manifest
            .entries
            .iter()
            .find(|e| e.rel_path_bytes == b"withx.txt")
            .unwrap();
        assert!(
            e.xattrs
                .iter()
                .any(|(n, v)| n == b"user.sentinel" && v == b"v500a"),
            "walk must capture the xattr"
        );

        let dest = tempfile::tempdir().unwrap();
        rehydrate(
            &out.manifest,
            dest.path(),
            &plane,
            &RestorePolicy::default(),
        )
        .unwrap();
        let restored = read_xattrs(&dest.path().join("withx.txt"));
        assert!(
            restored
                .iter()
                .any(|(n, v)| n == b"user.sentinel" && v == b"v500a"),
            "xattr must survive the round-trip"
        );
    }

    #[test]
    #[ignore = "needs a filesystem with sparse-file support (VM ext4, not tmpfs)"]
    fn tier2_sparse_roundtrip() {
        use std::io::{Seek, SeekFrom};
        let (plane, _pd) = temp_plane();
        let home = tempfile::tempdir().unwrap();
        let fp = home.path().join("sparse.bin");
        {
            let mut f = File::create(&fp).unwrap();
            f.write_all(b"head").unwrap();
            f.seek(SeekFrom::Start(1_000_000)).unwrap();
            f.write_all(b"tail").unwrap();
            f.set_len(1_000_004).unwrap();
        }

        let out = walk_home(home.path(), &plane).unwrap();
        let e = out
            .manifest
            .entries
            .iter()
            .find(|e| e.rel_path_bytes == b"sparse.bin")
            .unwrap();
        assert!(
            e.content.len() >= 2,
            "sparse file should yield multiple data extents, got {}",
            e.content.len()
        );

        let dest = tempfile::tempdir().unwrap();
        rehydrate(
            &out.manifest,
            dest.path(),
            &plane,
            &RestorePolicy::default(),
        )
        .unwrap();
        assert_eq!(
            sha256_file(&fp),
            sha256_file(&dest.path().join("sparse.bin")),
            "sparse content must match logically"
        );
        let rm = std::fs::metadata(dest.path().join("sparse.bin")).unwrap();
        assert!(
            rm.blocks() * 512 < rm.size(),
            "restored file should preserve holes (be sparse)"
        );
    }

    fn set_test_xattr(path: &Path, name: &[u8], value: &[u8]) {
        let cp = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let cn = std::ffi::CString::new(name).unwrap();
        let rc = unsafe {
            libc::lsetxattr(
                cp.as_ptr(),
                cn.as_ptr(),
                value.as_ptr() as *const _,
                value.len(),
                0,
            )
        };
        assert_eq!(
            rc,
            0,
            "lsetxattr failed (filesystem may lack xattr support): {}",
            std::io::Error::last_os_error()
        );
    }
}
