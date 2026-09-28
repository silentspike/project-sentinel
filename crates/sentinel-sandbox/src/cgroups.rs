//! cgroups v2 resource limits and PSI monitoring.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use tracing::{info, warn};

// PsiMetrics and parse_psi live in sentinel-common for cross-crate reuse.
pub use sentinel_common::psi::{parse_psi, PsiMetrics};

/// Default mount path for agent storage (tmpfs on VM).
const AGENT_STORAGE_PATH: &str = "/ram";
/// Fallback mount path when /ram is not available.
const FALLBACK_STORAGE_PATH: &str = "/";
const CGROUP_ROOT: &str = "/sys/fs/cgroup/sentinel";
const REQUIRED_CONTROLLERS: [&str; 4] = ["cpu", "memory", "pids", "io"];
// Fail closed rather than walking an unbounded delegated hierarchy.
const MAX_CGROUP_DEPTH: usize = 16;
const MAX_CGROUP_DIRS: usize = 256;
const MAX_CGROUP_ENTRIES: usize = 32_768;
const MAX_CONTROL_BYTES: u64 = 1_048_576;
const MAX_CGROUP_PIDS: usize = 131_072;

/// Ressourcen-Profil fuer dynamische cgroup-Limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    /// Wartet auf LLM-Response, keine Aktivitaet
    Idle,
    /// Standard-Arbeit
    Normal,
    /// Phase 2: npm install, cargo build, intensive I/O
    Heavy,
    /// Phase 2: Bio-Pause (Toilette, Essen)
    Suspended,
}

impl ResourceProfile {
    /// Gibt die cgroup-Limits fuer dieses Profil zurueck.
    pub fn limits(&self) -> CgroupLimits {
        match self {
            Self::Idle => CgroupLimits {
                cpu_quota_us: 25_000,
                cpu_period_us: 100_000,
                memory_bytes: 128 * 1024 * 1024,
                process_count: 32,
                io_max_iops: 100,
                io_max_bps: 5 * 1024 * 1024,
            },
            Self::Normal => CgroupLimits::default(),
            Self::Heavy => CgroupLimits {
                cpu_quota_us: 200_000,
                cpu_period_us: 100_000,
                memory_bytes: 512 * 1024 * 1024,
                process_count: 128,
                io_max_iops: 1000,
                io_max_bps: 50 * 1024 * 1024,
            },
            Self::Suspended => CgroupLimits {
                cpu_quota_us: 10_000,
                cpu_period_us: 100_000,
                memory_bytes: 64 * 1024 * 1024,
                process_count: 8,
                io_max_iops: 50,
                io_max_bps: 2 * 1024 * 1024,
            },
        }
    }
}

impl std::fmt::Display for ResourceProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Normal => write!(f, "normal"),
            Self::Heavy => write!(f, "heavy"),
            Self::Suspended => write!(f, "suspended"),
        }
    }
}

/// Resource limits fuer cgroups v2.
#[derive(Debug, Clone)]
pub struct CgroupLimits {
    pub cpu_quota_us: u64,  // 100000 = 100% einer CPU
    pub cpu_period_us: u64, // 100000 = 100ms
    pub memory_bytes: u64,  // 256 * 1024 * 1024 = 256MB
    pub process_count: u32, // cgroup pids.max
    pub io_max_iops: u32,   // 300
    pub io_max_bps: u64,    // 10 * 1024 * 1024 = 10MB/s
}

impl Default for CgroupLimits {
    fn default() -> Self {
        Self {
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            memory_bytes: 256 * 1024 * 1024,
            process_count: 64,
            io_max_iops: 300,
            io_max_bps: 10 * 1024 * 1024,
        }
    }
}

/// Result of creating a cgroup — tracks which controllers are available.
#[derive(Debug)]
pub struct CgroupSetup {
    /// Whether the IO controller is delegated and io.max can be enforced.
    pub io_available: bool,
}

/// Erzeugt den cgroup v2 Pfad fuer einen Agenten.
pub fn cgroup_path(name: &str) -> String {
    format!("/sys/fs/cgroup/sentinel/{name}")
}

/// Returns the cgroup inode (used as cgroup_id key in eBPF maps).
///
/// eBPF helper `bpf_get_current_cgroup_id()` returns the inode of the
/// cgroup directory. We use `stat().ino()` to get the same value from userspace.
pub fn cgroup_id(name: &str) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let path = cgroup_path(name);
    std::fs::metadata(&path).ok().map(|m| m.ino())
}

/// Returns the runtime leaf inode after workbench preparation.
/// Unlike `cgroup_id`, this never returns the cumulative agent parent inode.
pub fn runtime_cgroup_id(name: &str) -> Option<u64> {
    let agent = open_agent_at(Path::new(CGROUP_ROOT), name).ok()??;
    let runtime = prepared_runtime(&agent).ok()??;
    runtime.metadata().ok().map(|m| m.ino())
}

/// Prepares an existing, empty agent parent for workbench command isolation.
///
/// Limits stay cumulative on the agent parent. Runtime processes belong in
/// `runtime`; individual command cgroups belong beneath the returned `commands`
/// directory. Call before adding any runtime PID, and attest the runtime inode
/// separately with `runtime_cgroup_id`. Errors retain the tree for reconciliation.
/// The pinned agent directory must reside on a cgroup v2 filesystem.
pub fn prepare_workbench_cgroup(name: &str) -> Result<PathBuf> {
    prepare_kernel_workbench_cgroup_at(Path::new(CGROUP_ROOT), name)
}

fn prepare_kernel_workbench_cgroup_at(root: &Path, name: &str) -> Result<PathBuf> {
    let agent = open_agent_at(root, name)?.context("Agent cgroup does not exist")?;
    require_cgroup_v2(&agent)?;
    prepare_workbench_cgroup_tree(&agent)?;
    Ok(root.join(name).join("commands"))
}

fn require_cgroup_v2(agent: &File) -> Result<()> {
    use nix::sys::statfs::{fstatfs, CGROUP2_SUPER_MAGIC};

    let filesystem = fstatfs(agent).context("Failed to inspect agent cgroup filesystem")?;
    anyhow::ensure!(
        filesystem.filesystem_type() == CGROUP2_SUPER_MAGIC,
        "Agent directory is not on a cgroup v2 filesystem"
    );
    Ok(())
}

#[cfg(test)]
fn prepare_workbench_cgroup_at(root: &Path, name: &str) -> Result<PathBuf> {
    // Ordinary-directory fixtures exercise tree logic, not kernel attestation.
    let agent = open_agent_at(root, name)?.context("Agent cgroup does not exist")?;
    prepare_workbench_cgroup_tree(&agent)?;
    Ok(root.join(name).join("commands"))
}

fn prepare_workbench_cgroup_tree(agent: &File) -> Result<()> {
    // All read-only preconditions precede delegation or directory creation.
    require_empty(agent)?;
    require_controllers(agent, "cgroup.controllers")?;
    let tree = inspect_cgroup_tree(agent)?;
    for (index, node) in tree.iter().enumerate().skip(1) {
        if node.parent == Some(0) {
            anyhow::ensure!(
                node.name == "runtime" || node.name == "commands",
                "Unexpected child of workbench agent: {}",
                node.name
            );
        }
        if node.name == "runtime" && node.parent == Some(0) {
            anyhow::ensure!(
                !tree.iter().any(|child| child.parent == Some(index)),
                "Runtime cgroup must be a leaf"
            );
        }
    }
    if let Some(commands) = open_optional_child(agent, "commands")? {
        require_empty(&commands)?;
        require_controllers(&commands, "cgroup.controllers")?;
    }
    enable_required_controllers(agent)?;
    let runtime = ensure_child(agent, "runtime")?;
    let commands = ensure_child(agent, "commands")?;
    enable_required_controllers(&commands)?;
    require_controllers(agent, "cgroup.subtree_control")?;
    require_controllers(&commands, "cgroup.subtree_control")?;
    // Confirm the leaf exists without moving legacy processes or altering limits.
    read_members(&runtime)?;
    Ok(())
}

fn validate_cgroup_name(name: &str) -> Result<()> {
    // Preserve employee names verbatim; confinement needs one UTF-8 component,
    // not a slug alphabet. The kernel's name limit is in bytes, not characters.
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.chars().any(char::is_control),
        "Invalid immediate cgroup child name: {name:?}"
    );
    Ok(())
}

// Pin each directory with O_NOFOLLOW. Child paths use only our own descriptor
// and a validated single component, so renames cannot redirect traversal.
fn fd_path(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}

fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn open_root(root: &Path) -> std::io::Result<File> {
    let mut dir = open_directory(Path::new("/"))?;
    if !root.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Cgroup root must be absolute",
        ));
    }
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => dir = open_directory(&fd_path(&dir).join(name))?,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid cgroup root",
                ));
            }
        }
    }
    Ok(dir)
}

fn open_optional_child(parent: &File, name: &str) -> Result<Option<File>> {
    validate_cgroup_name(name)?;
    match open_directory(&fd_path(parent).join(name)) {
        Ok(dir) => {
            anyhow::ensure!(
                dir.metadata()?.dev() == parent.metadata()?.dev(),
                "Cgroup child crosses a filesystem boundary"
            );
            Ok(Some(dir))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to open cgroup child {name}")),
    }
}

fn open_agent_at(root: &Path, name: &str) -> Result<Option<File>> {
    validate_cgroup_name(name)?;
    let root = match open_root(root) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("Failed to open owned cgroup root"),
    };
    open_optional_child(&root, name)
}

fn ensure_child(parent: &File, name: &str) -> Result<File> {
    if let Some(dir) = open_optional_child(parent, name)? {
        return Ok(dir);
    }
    std::fs::create_dir(fd_path(parent).join(name))
        .with_context(|| format!("Failed to create cgroup child {name}"))?;
    open_optional_child(parent, name)?.context("Created cgroup child disappeared")
}

fn open_control(dir: &File, name: &str, write: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(!write)
        .write(write)
        .truncate(write)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(fd_path(dir).join(name))
        .with_context(|| format!("Failed to open cgroup control {name}"))?;
    anyhow::ensure!(file.metadata()?.is_file(), "Invalid cgroup control {name}");
    Ok(file)
}

fn read_control(dir: &File, name: &str) -> Result<String> {
    let mut content = String::new();
    open_control(dir, name, false)?
        .take(MAX_CONTROL_BYTES + 1)
        .read_to_string(&mut content)
        .with_context(|| format!("Failed to read cgroup control {name}"))?;
    anyhow::ensure!(
        content.len() as u64 <= MAX_CONTROL_BYTES,
        "Cgroup control {name} exceeds read bound"
    );
    Ok(content)
}

fn write_control(dir: &File, name: &str, value: &str) -> Result<()> {
    open_control(dir, name, true)?
        .write_all(value.as_bytes())
        .with_context(|| format!("Failed to write cgroup control {name}"))
}

fn require_empty(dir: &File) -> Result<()> {
    anyhow::ensure!(
        read_members(dir)?.is_empty(),
        "Cgroup has direct processes; preparation requires an empty parent"
    );
    Ok(())
}

fn require_controllers(dir: &File, control: &str) -> Result<()> {
    let content = read_control(dir, control)?;
    for required in REQUIRED_CONTROLLERS {
        anyhow::ensure!(
            content.split_whitespace().any(|c| c == required),
            "Required controller {required} missing from {control}"
        );
    }
    Ok(())
}

fn enable_required_controllers(dir: &File) -> Result<()> {
    require_empty(dir)?;
    require_controllers(dir, "cgroup.controllers")?;
    if require_controllers(dir, "cgroup.subtree_control").is_err() {
        write_control(dir, "cgroup.subtree_control", "+cpu +memory +pids +io")?;
    }
    require_controllers(dir, "cgroup.subtree_control")
}

fn prepared_runtime(agent: &File) -> Result<Option<File>> {
    let runtime = open_optional_child(agent, "runtime")?;
    let commands = open_optional_child(agent, "commands")?;
    match (runtime, commands) {
        (None, None) => Ok(None),
        (Some(runtime), Some(commands)) => {
            require_empty(agent)?;
            require_empty(&commands)?;
            require_controllers(agent, "cgroup.subtree_control")?;
            require_controllers(&commands, "cgroup.subtree_control")?;
            read_members(&runtime)?;
            anyhow::ensure!(
                inspect_cgroup_tree(&runtime)?.len() == 1,
                "Runtime cgroup must be a leaf"
            );
            Ok(Some(runtime))
        }
        _ => anyhow::bail!("Incomplete workbench cgroup hierarchy"),
    }
}

struct CgroupNode {
    dir: File,
    parent: Option<usize>,
    name: String,
    depth: usize,
}

fn inspect_cgroup_tree(agent: &File) -> Result<Vec<CgroupNode>> {
    let mut tree = vec![CgroupNode {
        dir: agent.try_clone()?,
        parent: None,
        name: String::new(),
        depth: 0,
    }];
    let mut entries = 0;
    let mut index = 0;
    while index < tree.len() {
        for entry in std::fs::read_dir(fd_path(&tree[index].dir))? {
            let entry = entry?;
            entries += 1;
            anyhow::ensure!(entries <= MAX_CGROUP_ENTRIES, "Cgroup entry bound exceeded");
            let kind = entry.file_type()?;
            anyhow::ensure!(!kind.is_symlink(), "Symlink in cgroup hierarchy");
            if !kind.is_dir() {
                anyhow::ensure!(kind.is_file(), "Unexpected cgroup entry type");
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Non-UTF-8 cgroup child name"))?;
            validate_cgroup_name(&name)?;
            let depth = tree[index].depth + 1;
            anyhow::ensure!(depth <= MAX_CGROUP_DEPTH, "Cgroup depth bound exceeded");
            anyhow::ensure!(
                tree.len() < MAX_CGROUP_DIRS,
                "Cgroup directory bound exceeded"
            );
            let dir = open_optional_child(&tree[index].dir, &name)?
                .context("Cgroup child disappeared during inspection")?;
            // Ordinary directories are not members of the cgroup hierarchy.
            read_members(&dir)?;
            tree.push(CgroupNode {
                dir,
                parent: Some(index),
                name,
                depth,
            });
        }
        index += 1;
    }
    Ok(tree)
}

fn read_members(dir: &File) -> Result<Vec<u32>> {
    parse_cgroup_pids(&read_control(dir, "cgroup.procs")?)
}

fn list_members(agent: &File) -> Result<Vec<u32>> {
    let tree = inspect_cgroup_tree(agent)?;
    let mut pids = Vec::new();
    for node in tree {
        pids.extend(read_members(&node.dir)?);
        anyhow::ensure!(pids.len() <= MAX_CGROUP_PIDS, "Cgroup PID bound exceeded");
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

/// Discovers the whole-disk block device (major:minor) backing a given mount path.
///
/// Parses `/proc/self/mountinfo` to find the device for the filesystem
/// containing `mount_path`. If the device is a partition (e.g. `8:1` = sda1),
/// resolves it to the whole disk device (e.g. `8:0` = sda) because cgroup v2
/// `io.max` only accepts whole-disk devices.
///
/// Falls back to `/` if `mount_path` is not found.
pub fn discover_block_device(mount_path: &str) -> Option<String> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let dev = discover_device_from_mountinfo(&mountinfo, mount_path)
        .or_else(|| discover_device_from_mountinfo(&mountinfo, FALLBACK_STORAGE_PATH))?;
    // Resolve partition to whole-disk device for io.max compatibility
    Some(resolve_to_whole_disk(&dev).unwrap_or(dev))
}

/// Resolves a partition device (e.g. `8:1`) to its whole-disk device (e.g. `8:0`).
///
/// Uses `/sys/dev/block/MAJ:MIN/partition` to detect partitions and reads
/// the parent device from `/sys/dev/block/MAJ:MIN/../dev`.
fn resolve_to_whole_disk(dev: &str) -> Option<String> {
    let partition_path = format!("/sys/dev/block/{dev}/partition");
    if std::path::Path::new(&partition_path).exists() {
        // It's a partition — read the parent (whole disk) device number
        let parent_dev_path = format!("/sys/dev/block/{dev}/../dev");
        let parent_dev = std::fs::read_to_string(parent_dev_path).ok()?;
        Some(parent_dev.trim().to_string())
    } else {
        None // Already a whole-disk device
    }
}

/// Parses mountinfo content to find the device for a specific mount point.
///
/// mountinfo format (per line):
/// `ID PARENT_ID MAJ:MIN ROOT MOUNT_POINT OPTIONS ... - FS_TYPE SOURCE OPTIONS`
/// Field 3 (0-indexed: 2) is the device in `major:minor` format.
/// Field 5 (0-indexed: 4) is the mount point.
fn discover_device_from_mountinfo(mountinfo: &str, mount_point: &str) -> Option<String> {
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 5 && fields[4] == mount_point {
            return Some(fields[2].to_string());
        }
    }
    None
}

/// Delegates required controllers (cpu, memory, pids, io) to child cgroups.
///
/// Must be called on the sentinel cgroup root BEFORE creating agent child cgroups.
/// Each controller is enabled individually (some may fail, e.g. IO not delegated from root).
pub fn delegate_controllers(cgroup_path: &str) -> bool {
    let subtree_path = format!("{cgroup_path}/cgroup.subtree_control");
    let mut any_ok = false;
    // Enable one at a time — kernel may reject combined writes if one controller is unavailable
    for controller in ["+cpu", "+memory", "+pids", "+io"] {
        match std::fs::write(&subtree_path, controller) {
            Ok(_) => {
                info!("Enabled {controller} controller in {cgroup_path}");
                any_ok = true;
            }
            Err(e) => {
                warn!("Failed to enable {controller} controller in {subtree_path}: {e}");
            }
        }
    }
    any_ok
}

/// Enables the IO controller in a cgroup's subtree_control.
///
/// Writes `"+io"` to `{cgroup_path}/cgroup.subtree_control`.
/// Best-effort: returns `true` on success, `false` on failure.
pub fn enable_io_controller(parent_cgroup_path: &str) -> bool {
    let subtree_path = format!("{parent_cgroup_path}/cgroup.subtree_control");
    match std::fs::write(&subtree_path, "+io") {
        Ok(_) => {
            info!("Enabled IO controller in {subtree_path}");
            true
        }
        Err(e) => {
            warn!("Failed to enable IO controller in {subtree_path}: {e}");
            false
        }
    }
}

/// Checks whether the IO controller is enabled in a cgroup's subtree_control.
pub fn io_controller_enabled(cgroup_path: &str) -> bool {
    let subtree_path = format!("{cgroup_path}/cgroup.subtree_control");
    std::fs::read_to_string(&subtree_path)
        .map(|s| s.split_whitespace().any(|c| c == "io"))
        .unwrap_or(false)
}

/// Formats an `io.max` line for cgroups v2.
///
/// Format: `"MAJ:MIN rbps=X wbps=X riops=Y wiops=Y"`
pub fn format_io_max(device: &str, limits: &CgroupLimits) -> String {
    format!(
        "{device} rbps={bps} wbps={bps} riops={iops} wiops={iops}",
        bps = limits.io_max_bps,
        iops = limits.io_max_iops
    )
}

/// Creates a cgroup v2 for an agent with resource limits.
///
/// Creates the cgroup directory and writes cpu, memory, and io limits.
/// IO limits are best-effort — if the IO controller is not delegated, they are skipped.
pub fn create_cgroup(name: &str, limits: &CgroupLimits) -> Result<CgroupSetup> {
    let path = cgroup_path(name);
    std::fs::create_dir_all(&path)
        .with_context(|| format!("Failed to create cgroup dir {path}"))?;

    // CPU quota
    let cpu_max = format!("{} {}", limits.cpu_quota_us, limits.cpu_period_us);
    std::fs::write(format!("{path}/cpu.max"), &cpu_max)
        .with_context(|| format!("Failed to write cpu.max for {name}"))?;
    info!("cgroup {name}: cpu.max = {cpu_max}");

    // Memory limit
    std::fs::write(
        format!("{path}/memory.max"),
        limits.memory_bytes.to_string(),
    )
    .with_context(|| format!("Failed to write memory.max for {name}"))?;

    std::fs::write(format!("{path}/pids.max"), limits.process_count.to_string())
        .with_context(|| format!("Failed to write pids.max for {name}"))?;
    info!("cgroup {name}: pids.max = {}", limits.process_count);

    // IO limits (best-effort — controller may not be delegated)
    let io_max_path = format!("{path}/io.max");
    let io_available = if std::path::Path::new(&io_max_path).exists() {
        match discover_block_device(AGENT_STORAGE_PATH) {
            Some(device) => {
                let io_max = format_io_max(&device, limits);
                match std::fs::write(&io_max_path, &io_max) {
                    Ok(_) => {
                        info!("cgroup {name}: io.max = {io_max}");
                        true
                    }
                    Err(e) => {
                        warn!(
                            "cgroup {name}: io.max write failed (controller not delegated?): {e}"
                        );
                        false
                    }
                }
            }
            None => {
                warn!("cgroup {name}: block device discovery failed, skipping io.max");
                false
            }
        }
    } else {
        warn!("cgroup {name}: io.max not available (IO controller not delegated)");
        false
    };

    Ok(CgroupSetup { io_available })
}

/// Resizes cgroup limits for a running agent (Hot-Resize).
///
/// Writes directly to cgroup v2 pseudo-files — takes effect immediately
/// without restarting the agent process. Memory limit is never set below
/// current usage + 16 MB safety margin to prevent OOM kills.
pub fn resize_cgroup(name: &str, limits: &CgroupLimits) -> Result<()> {
    let path = cgroup_path(name);
    if !std::path::Path::new(&path).exists() {
        return Err(anyhow::anyhow!("cgroup path does not exist: {path}"));
    }

    // CPU quota — sofort wirksam
    std::fs::write(
        format!("{path}/cpu.max"),
        format!("{} {}", limits.cpu_quota_us, limits.cpu_period_us),
    )
    .with_context(|| format!("Failed to resize cpu.max for {name}"))?;

    // Memory — SICHERHEITS-CHECK: nie unter memory.current + 16MB setzen
    let current_bytes = std::fs::read_to_string(format!("{path}/memory.current"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let safe_max = limits.memory_bytes.max(current_bytes + 16 * 1024 * 1024);
    std::fs::write(format!("{path}/memory.max"), safe_max.to_string())
        .with_context(|| format!("Failed to resize memory.max for {name}"))?;

    std::fs::write(format!("{path}/pids.max"), limits.process_count.to_string())
        .with_context(|| format!("Failed to resize pids.max for {name}"))?;

    // IO — best-effort (Controller muss delegiert sein)
    let io_max_path = format!("{path}/io.max");
    if std::path::Path::new(&io_max_path).exists() {
        if let Some(device) = discover_block_device(AGENT_STORAGE_PATH) {
            let io_max = format_io_max(&device, limits);
            let _ = std::fs::write(&io_max_path, &io_max);
        }
    }

    Ok(())
}

/// Removes a cgroup for an agent.
///
/// Removes bounded descendants bottom-up, using rmdir only. Populated or
/// malformed trees fail preflight; any removal failure retains the agent root.
/// A missing cgroup is a successful no-op.
pub fn remove_cgroup(name: &str) -> Result<()> {
    remove_cgroup_at(Path::new(CGROUP_ROOT), name, |path| {
        std::fs::remove_dir(path)
    })
}

fn remove_cgroup_at(
    root: &Path,
    name: &str,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<()> {
    validate_cgroup_name(name)?;
    let owner = match open_root(root) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("Failed to open owned cgroup root"),
    };
    let Some(agent) = open_optional_child(&owner, name)? else {
        return Ok(());
    };
    let tree = inspect_cgroup_tree(&agent)?;
    // Preflight the entire bounded tree before deleting even an empty child.
    for node in &tree {
        anyhow::ensure!(
            read_members(&node.dir)?.is_empty(),
            "Cannot remove populated cgroup {}",
            node.name
        );
    }
    for node in tree.iter().rev() {
        let (parent, child) = match node.parent {
            Some(index) => (&tree[index].dir, node.name.as_str()),
            None => (&owner, name),
        };
        let path = fd_path(parent).join(child);
        let current = std::fs::symlink_metadata(&path)?;
        let opened = node.dir.metadata()?;
        anyhow::ensure!(
            current.is_dir() && current.dev() == opened.dev() && current.ino() == opened.ino(),
            "Cgroup identity changed before removal"
        );
        remove(&path).with_context(|| format!("Failed to remove cgroup child {child}"))?;
    }
    info!("Removed cgroup for {name}");
    Ok(())
}

/// Force-kills all processes that are still attached to an agent cgroup.
///
/// Runtime reconciliation uses this for stale cgroups whose tracked runtime
/// handle is already gone. `cgroup.kill` handles stopped processes reliably;
/// the PID fallback keeps older cgroup-v2 hosts functional.
pub fn kill_cgroup_processes(name: &str) -> Result<usize> {
    let Some(agent) = open_agent_at(Path::new(CGROUP_ROOT), name)? else {
        return Ok(0);
    };

    kill_members(&agent)
}

fn kill_members(agent: &File) -> Result<usize> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    let initial_pids = list_members(agent)?;
    let kill_path = fd_path(agent).join("cgroup.kill");
    let has_kill = match std::fs::symlink_metadata(&kill_path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Invalid cgroup.kill control"
            );
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).context("Failed to inspect cgroup.kill"),
    };
    let mut members = initial_pids.clone();
    for _ in 0..20 {
        if has_kill {
            // cgroup.kill covers descendants atomically, including stopped tasks.
            // Write even when the initial parent/member snapshot was empty.
            write_control(agent, "cgroup.kill", "1")?;
        } else {
            // Re-scan on every pass: children can fork while a prior pass dies.
            for pid in &members {
                match kill(Pid::from_raw(*pid as i32), Signal::SIGKILL) {
                    Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("Failed to SIGKILL cgroup member {pid}"));
                    }
                }
            }
        }
        members = list_members(agent)?;
        if members.is_empty() {
            return Ok(initial_pids.len());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let remaining = list_members(agent)?;
    if remaining.is_empty() {
        Ok(initial_pids.len())
    } else {
        Err(anyhow::anyhow!(
            "Cgroup still contains PIDs after kill: {remaining:?}"
        ))
    }
}

/// Adds a process to the prepared runtime leaf, or the legacy agent parent.
pub fn add_pid_to_cgroup(name: &str, pid: u32) -> Result<()> {
    let agent =
        open_agent_at(Path::new(CGROUP_ROOT), name)?.context("Agent cgroup does not exist")?;
    add_member(&agent, pid).with_context(|| format!("Failed to add PID {pid} to cgroup {name}"))
}

fn add_member(agent: &File, pid: u32) -> Result<()> {
    anyhow::ensure!(
        pid > 0 && pid <= i32::MAX as u32,
        "Invalid cgroup PID {pid}"
    );
    let runtime = prepared_runtime(agent)?;
    write_control(
        runtime.as_ref().unwrap_or(agent),
        "cgroup.procs",
        &pid.to_string(),
    )
}

/// Lists sorted, unique PIDs in an agent cgroup and its bounded descendants.
/// Malformed membership files, symlinks, and exceeded bounds are errors.
pub fn list_pids_in_cgroup(name: &str) -> Result<Vec<u32>> {
    let agent =
        open_agent_at(Path::new(CGROUP_ROOT), name)?.context("Agent cgroup does not exist")?;
    list_members(&agent)
}

fn parse_cgroup_pids(content: &str) -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    for line in content.split_terminator('\n') {
        anyhow::ensure!(
            !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()),
            "Malformed cgroup PID: {line:?}"
        );
        let pid = line.parse::<u32>().context("Cgroup PID overflow")?;
        anyhow::ensure!(
            pid > 0 && pid <= i32::MAX as u32,
            "Invalid cgroup PID {pid}"
        );
        anyhow::ensure!(pids.len() < MAX_CGROUP_PIDS, "Cgroup PID bound exceeded");
        pids.push(pid);
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

/// Sets the OOM score adjustment for a process.
///
/// -1000 = immortal (ECS core), +1000 = first to kill.
pub fn set_oom_score(pid: u32, score: i32) -> Result<()> {
    let path = format!("/proc/{pid}/oom_score_adj");
    std::fs::write(&path, score.to_string())
        .with_context(|| format!("Failed to set oom_score_adj for PID {pid}"))
}

/// Reads PSI metrics from an agent's cgroup.
///
/// resource: "cpu", "memory", or "io"
pub fn read_psi_from_cgroup(name: &str, resource: &str) -> Result<PsiMetrics> {
    let path = format!("{}/{resource}.pressure", cgroup_path(name));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read PSI {resource} for cgroup {name}"))?;
    parse_psi(&content).with_context(|| format!("Failed to parse PSI {resource} for cgroup {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let limits = CgroupLimits::default();
        assert_eq!(limits.cpu_quota_us, 100_000);
        assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.io_max_iops, 300);
        assert_eq!(limits.io_max_bps, 10 * 1024 * 1024);
    }

    #[test]
    fn cgroup_default_limits() {
        let limits = CgroupLimits::default();
        assert_eq!(limits.cpu_quota_us, 100_000);
        assert_eq!(limits.cpu_period_us, 100_000);
        assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.io_max_iops, 300);
        assert_eq!(limits.io_max_bps, 10 * 1024 * 1024);
    }

    #[test]
    fn cgroup_path_format() {
        assert_eq!(cgroup_path("thomas"), "/sys/fs/cgroup/sentinel/thomas");
    }

    #[test]
    fn psi_parse() {
        let content =
            "some avg10=1.50 avg60=2.30 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0";
        let metrics = parse_psi(content).unwrap();
        assert_eq!(metrics.avg10, 1.50);
        assert_eq!(metrics.avg60, 2.30);
        assert_eq!(metrics.avg300, 0.10);
        assert_eq!(metrics.total, 12345);
    }

    #[test]
    fn cgroup_setup_fields() {
        let setup = CgroupSetup { io_available: true };
        assert!(setup.io_available);
    }

    #[test]
    #[ignore] // Needs cgroup v2 root access (VM only)
    fn cgroup_create_remove() {
        let limits = CgroupLimits::default();
        let setup = create_cgroup("test-agent", &limits).unwrap();
        assert!(std::path::Path::new("/sys/fs/cgroup/sentinel/test-agent").exists());
        remove_cgroup("test-agent").unwrap();
        assert!(!std::path::Path::new("/sys/fs/cgroup/sentinel/test-agent").exists());
        let _ = setup;
    }

    #[test]
    #[ignore] // Needs /proc access
    fn read_psi_real() {
        // Echte PSI-Datei lesen (/proc/pressure/cpu)
    }

    #[test]
    fn remove_nonexistent_ok() {
        // Removing a non-existent cgroup should succeed silently
        assert!(remove_cgroup("does-not-exist-xyz").is_ok());
    }

    #[test]
    fn kill_nonexistent_cgroup_is_noop() {
        assert_eq!(kill_cgroup_processes("does-not-exist-xyz").unwrap(), 0);
    }

    #[test]
    fn discover_device_from_mountinfo_finds_root() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
                       30 22 0:26 / /tmp rw,nosuid,nodev shared:14 - tmpfs tmpfs rw\n\
                       45 22 252:0 / /ram rw,nosuid,nodev shared:20 - tmpfs tmpfs rw,size=2097152k";
        assert_eq!(
            discover_device_from_mountinfo(sample, "/"),
            Some("8:2".to_string())
        );
    }

    #[test]
    fn discover_device_from_mountinfo_finds_ram() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
                       45 22 252:0 / /ram rw,nosuid,nodev shared:20 - tmpfs tmpfs rw,size=2097152k";
        assert_eq!(
            discover_device_from_mountinfo(sample, "/ram"),
            Some("252:0".to_string())
        );
    }

    #[test]
    fn discover_device_from_mountinfo_not_found() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw";
        assert_eq!(discover_device_from_mountinfo(sample, "/nonexistent"), None);
    }

    #[test]
    fn format_io_max_includes_device() {
        let limits = CgroupLimits::default();
        let result = format_io_max("8:0", &limits);
        assert!(
            result.starts_with("8:0 "),
            "io.max must start with device, got: {result}"
        );
        assert!(result.contains("rbps=10485760"), "must contain rbps");
        assert!(result.contains("riops=300"), "must contain riops");
        assert!(result.contains("wiops=300"), "must contain wiops");
        assert!(result.contains("wbps=10485760"), "must contain wbps");
    }

    #[test]
    fn format_io_max_custom_limits() {
        let limits = CgroupLimits {
            io_max_iops: 500,
            io_max_bps: 20 * 1024 * 1024,
            ..CgroupLimits::default()
        };
        let result = format_io_max("252:0", &limits);
        assert_eq!(
            result,
            "252:0 rbps=20971520 wbps=20971520 riops=500 wiops=500"
        );
    }

    #[test]
    fn io_controller_enabled_false_for_nonexistent() {
        assert!(!io_controller_enabled(
            "/sys/fs/cgroup/nonexistent-sentinel-test"
        ));
    }

    #[test]
    fn discover_block_device_reads_proc() {
        // /proc/self/mountinfo should always exist on Linux
        if std::path::Path::new("/proc/self/mountinfo").exists() {
            // Root filesystem should always have a device
            let device = discover_block_device("/");
            assert!(device.is_some(), "should discover device for /");
            let dev = device.unwrap();
            assert!(
                dev.contains(':'),
                "device should be in MAJ:MIN format, got: {dev}"
            );
        }
    }

    #[test]
    fn resolve_to_whole_disk_none_for_nonexistent() {
        // A device that doesn't exist should return None
        assert_eq!(resolve_to_whole_disk("999:999"), None);
    }

    #[test]
    fn resolve_to_whole_disk_on_real_system() {
        // On real systems, check if 8:1 resolves to 8:0 (sda1 -> sda)
        if std::path::Path::new("/sys/dev/block/8:1/partition").exists() {
            let parent = resolve_to_whole_disk("8:1");
            assert!(parent.is_some(), "8:1 should resolve to parent device");
            assert_eq!(
                parent.unwrap(),
                "8:0",
                "sda1 (8:1) should resolve to sda (8:0)"
            );
        }
    }

    #[test]
    fn parse_cgroup_pids_dedups() {
        let pids = parse_cgroup_pids("42\n17\n42\n").unwrap();
        assert_eq!(pids, vec![17, 42]);
        assert!(parse_cgroup_pids("").unwrap().is_empty());
    }

    #[test]
    fn parse_cgroup_pids_rejects_malformed_members() {
        for content in [
            "invalid\n",
            "42\ninvalid\n",
            "\n",
            "0\n",
            "-1\n",
            "+1\n",
            " 42\n",
            "42 \n",
            "42\r\n",
            "2147483648\n",
            "4294967296\n",
        ] {
            assert!(parse_cgroup_pids(content).is_err(), "accepted {content:?}");
        }
    }

    fn fixture_cgroup(path: &Path, members: &str) {
        std::fs::create_dir(path).unwrap();
        std::fs::write(path.join("cgroup.procs"), members).unwrap();
        std::fs::write(path.join("cgroup.controllers"), "cpu memory pids io\n").unwrap();
        // Ordinary files cannot emulate kernel write/read-back delegation.
        std::fs::write(path.join("cgroup.subtree_control"), "cpu memory pids io\n").unwrap();
    }

    fn fixture_agent(root: &Path) -> File {
        fixture_cgroup(&root.join("agent"), "");
        open_agent_at(root, "agent").unwrap().unwrap()
    }

    #[test]
    fn preparation_rejects_invalid_names_without_changes() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "",
            ".",
            "..",
            "../agent",
            "agent/child",
            "/agent",
            "agent/..",
            "agent/../other",
            "agent\0",
            "agent\n",
            "agent\t",
            "Thomas Mueller/child",
        ] {
            assert!(prepare_workbench_cgroup_at(root.path(), name).is_err());
            assert!(remove_cgroup_at(root.path(), name, |p| std::fs::remove_dir(p)).is_err());
        }
        assert!(validate_cgroup_name(&"a".repeat(256)).is_err());
        assert!(validate_cgroup_name("agent-123_abc").is_ok());
        assert!(validate_cgroup_name("agent.prod").is_ok());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn existing_employee_names_remain_exact_utf8_components() {
        for name in [
            "Thomas Mueller",
            "Dr. Katharina Wiesner",
            "Katharina \"Kathi\" Wiegand",
            "J\u{00f6}rg M\u{00fc}ller",
            "\u{674e}\u{660e}",
            ".hidden",
            "agent\\child",
        ] {
            let root = tempfile::tempdir().unwrap();
            validate_cgroup_name(name).unwrap();
            let path = root.path().join(name);
            fixture_cgroup(&path, "");
            assert_eq!(cgroup_path(name), format!("{CGROUP_ROOT}/{name}"));
            let agent = open_agent_at(root.path(), name).unwrap().unwrap();
            add_member(&agent, 42).unwrap();
            assert_eq!(list_members(&agent).unwrap(), vec![42]);
            write_control(&agent, "cgroup.procs", "").unwrap();
            fixture_cgroup(&path.join("runtime"), "");
            fixture_cgroup(&path.join("commands"), "");
            assert_eq!(
                prepare_workbench_cgroup_at(root.path(), name).unwrap(),
                path.join("commands")
            );
            add_member(&agent, 17).unwrap();
            fixture_cgroup(&path.join("commands").join(name), "99\n");
            assert_eq!(list_members(&agent).unwrap(), vec![17, 99]);
            assert!(remove_cgroup_at(root.path(), name, |_| {
                panic!("populated employee tree must survive cleanup")
            })
            .is_err());
            assert!(path.join("commands").join(name).exists());
            let runtime = prepared_runtime(&agent).unwrap().unwrap();
            write_control(&runtime, "cgroup.procs", "").unwrap();
            std::fs::write(path.join("commands").join(name).join("cgroup.procs"), "").unwrap();
            remove_cgroup_at(root.path(), name, |directory| {
                // Fixture-only adapter: production cleanup remains rmdir-only.
                for entry in std::fs::read_dir(directory)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        std::fs::remove_file(entry.path())?;
                    }
                }
                std::fs::remove_dir(directory)
            })
            .unwrap();
            assert!(!path.exists());
        }
        assert!(validate_cgroup_name(&"\u{00fc}".repeat(127)).is_ok());
        assert!(validate_cgroup_name(&"\u{00fc}".repeat(128)).is_err());
    }

    #[test]
    fn kernel_preparation_rejects_wrong_filesystem_before_changes() {
        for existing_leaves in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let agent = fixture_agent(root.path());
            let path = root.path().join("agent");
            if existing_leaves {
                fixture_cgroup(&path.join("runtime"), "");
                fixture_cgroup(&path.join("commands"), "");
            }
            // Fake controller lists, even in a complete ordinary tree, must
            // not satisfy the public entry's pinned-filesystem check.
            assert!(require_cgroup_v2(&agent).is_err());
            let before = read_control(&agent, "cgroup.subtree_control").unwrap();
            let error = prepare_kernel_workbench_cgroup_at(root.path(), "agent").unwrap_err();
            assert!(error.to_string().contains("not on a cgroup v2 filesystem"));
            assert_eq!(
                read_control(&agent, "cgroup.subtree_control").unwrap(),
                before
            );
            assert!(read_members(&agent).unwrap().is_empty());
            assert_eq!(path.join("runtime").exists(), existing_leaves);
            assert_eq!(path.join("commands").exists(), existing_leaves);
            if existing_leaves {
                // The fixture-only entry still supports ordinary directories.
                prepare_workbench_cgroup_at(root.path(), "agent").unwrap();
            }
        }
    }

    #[test]
    fn preparation_rejects_nonempty_or_malformed_parent_before_edits() {
        for members in ["42\n", "bad\n"] {
            let root = tempfile::tempdir().unwrap();
            fixture_agent(root.path());
            let agent = root.path().join("agent");
            std::fs::write(agent.join("cgroup.procs"), members).unwrap();
            std::fs::write(agent.join("cgroup.subtree_control"), "").unwrap();
            assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
            assert!(!agent.join("runtime").exists());
            assert!(!agent.join("commands").exists());
            assert_eq!(
                std::fs::read_to_string(agent.join("cgroup.subtree_control")).unwrap(),
                ""
            );
        }
    }

    #[test]
    fn preparation_requires_every_controller_before_edits() {
        for missing in REQUIRED_CONTROLLERS {
            let root = tempfile::tempdir().unwrap();
            fixture_agent(root.path());
            let agent = root.path().join("agent");
            let available = REQUIRED_CONTROLLERS
                .into_iter()
                .filter(|c| *c != missing)
                .collect::<Vec<_>>()
                .join(" ");
            std::fs::write(agent.join("cgroup.controllers"), available).unwrap();
            std::fs::write(agent.join("cgroup.subtree_control"), "").unwrap();
            assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
            assert!(!agent.join("runtime").exists());
            assert_eq!(
                std::fs::read_to_string(agent.join("cgroup.subtree_control")).unwrap(),
                ""
            );
        }
    }

    #[test]
    fn preparation_verifies_commands_and_preserves_parent_limits() {
        let root = tempfile::tempdir().unwrap();
        fixture_agent(root.path());
        let agent = root.path().join("agent");
        fixture_cgroup(&agent.join("runtime"), "");
        fixture_cgroup(&agent.join("commands"), "");
        for control in ["cpu.max", "memory.max", "pids.max", "io.max"] {
            std::fs::write(agent.join(control), "unchanged").unwrap();
        }
        assert_eq!(
            prepare_workbench_cgroup_at(root.path(), "agent").unwrap(),
            agent.join("commands")
        );
        for control in ["cpu.max", "memory.max", "pids.max", "io.max"] {
            assert_eq!(
                std::fs::read_to_string(agent.join(control)).unwrap(),
                "unchanged"
            );
        }
        std::fs::write(agent.join("commands/cgroup.controllers"), "cpu memory pids").unwrap();
        std::fs::write(agent.join("cgroup.subtree_control"), "").unwrap();
        assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
        assert_eq!(
            std::fs::read_to_string(agent.join("cgroup.subtree_control")).unwrap(),
            ""
        );
    }

    #[test]
    fn preparation_requires_actual_delegation_readback() {
        let root = tempfile::tempdir().unwrap();
        fixture_agent(root.path());
        let agent = root.path().join("agent");
        std::fs::write(agent.join("cgroup.subtree_control"), "").unwrap();
        // A plain file echoes +controller, not the kernel's enabled controller
        // names. A successful write alone must never mean preparation succeeded.
        assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
        assert!(!agent.join("runtime").exists());
        assert!(!agent.join("commands").exists());
    }

    #[test]
    fn preparation_rejects_unavailable_or_populated_commands_before_edits() {
        for missing in REQUIRED_CONTROLLERS {
            let root = tempfile::tempdir().unwrap();
            fixture_agent(root.path());
            let agent = root.path().join("agent");
            fixture_cgroup(&agent.join("commands"), "");
            let available = REQUIRED_CONTROLLERS
                .into_iter()
                .filter(|c| *c != missing)
                .collect::<Vec<_>>()
                .join(" ");
            std::fs::write(agent.join("commands/cgroup.controllers"), available).unwrap();
            std::fs::write(agent.join("cgroup.subtree_control"), "").unwrap();
            assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
            assert!(!agent.join("runtime").exists());
            assert_eq!(
                read_control(&open_directory(&agent).unwrap(), "cgroup.subtree_control").unwrap(),
                ""
            );
        }
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        fixture_cgroup(&root.path().join("agent/commands"), "42\n");
        write_control(&agent, "cgroup.subtree_control", "").unwrap();
        assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
        assert_eq!(read_control(&agent, "cgroup.subtree_control").unwrap(), "");
        assert!(!root.path().join("agent/runtime").exists());
    }

    #[test]
    fn membership_routes_runtime_only_after_preparation() {
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        add_member(&agent, 42).unwrap();
        assert_eq!(read_members(&agent).unwrap(), vec![42]);
        write_control(&agent, "cgroup.procs", "").unwrap();
        fixture_cgroup(&root.path().join("agent/runtime"), "");
        assert!(add_member(&agent, 42).is_err());
        assert!(read_members(&agent).unwrap().is_empty());
        fixture_cgroup(&root.path().join("agent/commands"), "");
        prepare_workbench_cgroup_at(root.path(), "agent").unwrap();
        add_member(&agent, 17).unwrap();
        assert!(read_members(&agent).unwrap().is_empty());
        let runtime = prepared_runtime(&agent).unwrap().unwrap();
        assert_eq!(read_members(&runtime).unwrap(), vec![17]);
        assert_ne!(
            runtime.metadata().unwrap().ino(),
            agent.metadata().unwrap().ino()
        );
        assert!(add_member(&agent, 0).is_err());
    }

    #[test]
    fn recursive_members_include_descendants_of_empty_parent() {
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        fixture_cgroup(&root.path().join("agent/runtime"), "42\n");
        fixture_cgroup(&root.path().join("agent/commands"), "");
        fixture_cgroup(&root.path().join("agent/commands/cmd-1"), "17\n42\n");
        fixture_cgroup(&root.path().join("agent/commands/cmd-1/child"), "99\n");
        assert_eq!(list_members(&agent).unwrap(), vec![17, 42, 99]);
        std::fs::write(
            root.path().join("agent/commands/cmd-1/cgroup.procs"),
            "bad\n",
        )
        .unwrap();
        assert!(list_members(&agent).is_err());
    }

    #[test]
    fn recursive_inspection_and_cleanup_reject_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        fixture_cgroup(&root.path().join("outside"), "42\n");
        symlink(
            root.path().join("outside"),
            root.path().join("agent/escape"),
        )
        .unwrap();
        assert!(list_members(&agent).is_err());
        assert!(prepare_workbench_cgroup_at(root.path(), "agent").is_err());
        let mut removed = false;
        assert!(remove_cgroup_at(root.path(), "agent", |_| {
            removed = true;
            Ok(())
        })
        .is_err());
        assert!(!removed);
        assert!(root.path().join("outside").exists());
        symlink(
            root.path().join("outside"),
            root.path().join("linked-agent"),
        )
        .unwrap();
        assert!(open_agent_at(root.path(), "linked-agent").is_err());
        std::fs::remove_file(root.path().join("agent/escape")).unwrap();
        std::fs::remove_file(root.path().join("agent/cgroup.procs")).unwrap();
        symlink(
            root.path().join("outside/cgroup.procs"),
            root.path().join("agent/cgroup.procs"),
        )
        .unwrap();
        assert!(list_members(&agent).is_err());
        assert!(add_member(&agent, 17).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("outside/cgroup.procs")).unwrap(),
            "42\n"
        );
    }

    #[test]
    fn recursive_inspection_and_cleanup_are_bounded() {
        for wide in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let agent = fixture_agent(root.path());
            let mut parent = root.path().join("agent");
            let count = if wide {
                MAX_CGROUP_DIRS
            } else {
                MAX_CGROUP_DEPTH + 1
            };
            for index in 0..count {
                let child = parent.join(format!("child-{index}"));
                fixture_cgroup(&child, "");
                if !wide {
                    parent = child;
                }
            }
            assert!(list_members(&agent).is_err());
            let mut removals = 0;
            assert!(remove_cgroup_at(root.path(), "agent", |_| {
                removals += 1;
                Ok(())
            })
            .is_err());
            assert_eq!(removals, 0);
        }
    }

    #[test]
    fn inspection_rejects_non_cgroup_directories_and_oversized_membership() {
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        let child = root.path().join("agent/not-a-cgroup");
        std::fs::create_dir(&child).unwrap();
        assert!(list_members(&agent).is_err());
        assert!(remove_cgroup_at(root.path(), "agent", |_| panic!("preflight must fail")).is_err());
        std::fs::remove_dir(child).unwrap();
        std::fs::write(
            root.path().join("agent/cgroup.procs"),
            "1".repeat(MAX_CONTROL_BYTES as usize + 1),
        )
        .unwrap();
        assert!(list_members(&agent).is_err());
    }

    #[test]
    fn owned_root_cannot_be_a_symlink_and_missing_cleanup_is_a_noop() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        fixture_cgroup(&root.path().join("owner"), "");
        fixture_cgroup(&root.path().join("owner/agent"), "");
        symlink(root.path().join("owner"), root.path().join("alias")).unwrap();
        assert!(open_agent_at(&root.path().join("alias"), "agent").is_err());
        assert!(remove_cgroup_at(&root.path().join("alias"), "agent", |_| {
            panic!("symlink root must fail")
        })
        .is_err());
        remove_cgroup_at(root.path(), "missing", |_| {
            panic!("missing tree must be a noop")
        })
        .unwrap();
        assert!(root.path().join("owner/agent").exists());
    }

    #[test]
    fn cleanup_is_bottom_up_and_leaves_sibling_trees_untouched() {
        let root = tempfile::tempdir().unwrap();
        fixture_agent(root.path());
        fixture_cgroup(&root.path().join("sibling"), "42\n");
        fixture_cgroup(&root.path().join("agent/commands"), "");
        fixture_cgroup(&root.path().join("agent/commands/cmd-1"), "");
        fixture_cgroup(&root.path().join("agent/runtime"), "");
        let mut removed = Vec::new();
        remove_cgroup_at(root.path(), "agent", |path| {
            removed.push(path.file_name().unwrap().to_str().unwrap().to_string());
            // Only this fixture adapter removes ordinary control files. Real
            // cgroupfs removal uses rmdir alone, never remove_dir_all/unlink.
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    std::fs::remove_file(entry.path())?;
                }
            }
            std::fs::remove_dir(path)
        })
        .unwrap();
        assert_eq!(removed.last().unwrap(), "agent");
        assert!(
            removed.iter().position(|n| n == "cmd-1").unwrap()
                < removed.iter().position(|n| n == "commands").unwrap()
        );
        assert!(!root.path().join("agent").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("sibling/cgroup.procs")).unwrap(),
            "42\n"
        );
    }

    #[test]
    fn cleanup_retains_busy_malformed_or_failed_tree() {
        for members in ["42\n", "bad\n", ""] {
            let root = tempfile::tempdir().unwrap();
            fixture_agent(root.path());
            fixture_cgroup(&root.path().join("agent/commands"), "");
            fixture_cgroup(&root.path().join("agent/commands/cmd-1"), members);
            let mut removals = 0;
            assert!(remove_cgroup_at(root.path(), "agent", |_| {
                removals += 1;
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            })
            .is_err());
            assert_eq!(removals, usize::from(members.is_empty()));
            assert!(root.path().join("agent/commands/cmd-1").exists());
        }
    }

    #[test]
    fn kill_control_is_used_even_when_parent_snapshot_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        fixture_cgroup(&root.path().join("agent/commands"), "");
        std::fs::write(root.path().join("agent/cgroup.kill"), "").unwrap();
        assert_eq!(kill_members(&agent).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(root.path().join("agent/cgroup.kill")).unwrap(),
            "1"
        );
    }

    #[test]
    fn kill_does_not_report_success_with_populated_descendants() {
        let root = tempfile::tempdir().unwrap();
        let agent = fixture_agent(root.path());
        fixture_cgroup(&root.path().join("agent/commands"), "");
        fixture_cgroup(&root.path().join("agent/commands/cmd-1"), "42\n");
        // Never signal fixture PIDs: this ordinary control file selects only
        // the hierarchy-kill path. It cannot clear membership as cgroupfs does.
        std::fs::write(root.path().join("agent/cgroup.kill"), "").unwrap();
        assert!(kill_members(&agent).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("agent/cgroup.kill")).unwrap(),
            "1"
        );
    }
}
