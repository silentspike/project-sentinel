//! Standalone sanitized LAB mount; never opens an existing runtime store.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{ensure, Context};
use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::{LayerManager, MAX_WORKSPACE_BUDGET_BYTES};
use sentinel_fs::metadata::MetadataStore;

const AGENT: &str = "AGENT-07";
const MIN_BUDGET: u64 = 1024 * 1024;

#[derive(Debug)]
struct Options {
    data: PathBuf,
    mountpoint: PathBuf,
    budget: u64,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut args = args.into_iter();
        let usage = "usage: sentinel-fs-lab-mount DATA_DIR MOUNTPOINT WORKSPACE_BUDGET_BYTES";
        let data = PathBuf::from(args.next().context(usage)?);
        let mountpoint = PathBuf::from(args.next().context(usage)?);
        let budget = args.next().context(usage)?;
        let budget = budget.to_str().context(usage)?.parse::<u64>()?;
        ensure!(args.next().is_none(), "{usage}");
        ensure!(
            (MIN_BUDGET..=MAX_WORKSPACE_BUDGET_BYTES).contains(&budget),
            "invalid workspace budget"
        );
        Ok(Self {
            data,
            mountpoint,
            budget,
        })
    }

    fn validate(&self) -> anyhow::Result<()> {
        // SAFETY: geteuid has no pointer arguments or mutable process state.
        let uid = unsafe { libc::geteuid() };
        for path in [&self.data, &self.mountpoint] {
            ensure!(
                path.is_absolute() && fs::canonicalize(path)? == *path,
                "directory must be absolute and canonical"
            );
            let metadata = fs::symlink_metadata(path)?;
            ensure!(
                metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o7777 == 0o700,
                "directory must be caller-owned and mode 0700"
            );
            ensure!(
                fs::read_dir(path)?.next().is_none(),
                "directory must be fresh and empty"
            );
        }
        ensure!(
            !self.data.starts_with(&self.mountpoint) && !self.mountpoint.starts_with(&self.data),
            "data and mount directories must not overlap"
        );
        Ok(())
    }
}

fn fresh_layer(options: &Options) -> anyhow::Result<LayerManager> {
    options.validate()?;
    // An exclusive marker prevents two helpers from initializing the same lease.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(options.data.join("lab-mount.lock"))?;
    let cas = CasStore::open(&options.data)?;
    let meta = MetadataStore::open(options.data.join("metadata.redb"))?;
    let plane = Arc::new(ArtifactPlane::open(options.data.join("home.redb"))?);
    let layer = LayerManager::with_artifact_plane(cas, meta, plane);
    layer.init_base_root()?;
    layer.ensure_agent_root(AGENT)?;
    let budget = layer.set_workspace_budget(AGENT, options.budget)?;
    ensure!(
        budget.used_bytes == 0
            && budget.limit_bytes == options.budget
            && budget.explicitly_configured,
        "fresh namespace budget was not established"
    );
    Ok(layer)
}

fn run() -> anyhow::Result<()> {
    let options = Options::parse(std::env::args_os().skip(1))?;
    let layer = Arc::new(fresh_layer(&options)?);
    // This is configuration evidence only; the lease owner verifies mount readiness.
    eprintln!(
        "{}",
        serde_json::json!({"event": "lab_mount_configured", "agent": AGENT, "workspace_budget_bytes": options.budget})
    );
    sentinel_fs::fuse::start_fuse_layer(layer, &options.mountpoint)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("sentinel-fs-lab-mount: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;

    fn options(root: &Path) -> Options {
        let data = root.join("data");
        let mountpoint = root.join("mount");
        for path in [&data, &mountpoint] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Options {
            data,
            mountpoint,
            budget: 4 * MIN_BUDGET,
        }
    }

    #[test]
    fn arguments_require_exact_paths_and_bounded_numeric_budget() {
        let args = |budget: &str| ["/data", "/mount", budget].map(OsString::from);
        assert_eq!(Options::parse(args("4194304")).unwrap().budget, 4194304);
        for budget in ["0", "1048575", "1099511627777", "invalid"] {
            assert!(Options::parse(args(budget)).is_err());
        }
        assert!(Options::parse([OsString::from("/data")]).is_err());
        assert!(
            Options::parse(["/data", "/mount", "4194304", "extra"].map(OsString::from)).is_err()
        );
    }

    #[test]
    fn rejects_occupied_or_nonprivate_directories_without_creating_stores() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        fs::write(options.data.join("retained"), b"not lab state").unwrap();
        assert!(fresh_layer(&options).is_err());
        assert!(!options.data.join("metadata.redb").exists());
        fs::remove_file(options.data.join("retained")).unwrap();
        fs::set_permissions(&options.mountpoint, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(fresh_layer(&options).is_err());
        assert!(!options.data.join("lab-mount.lock").exists());
    }

    #[test]
    fn rejects_symlink_and_overlapping_paths() {
        let root = tempfile::tempdir().unwrap();
        let mut options = options(root.path());
        let alias = root.path().join("alias");
        symlink(&options.mountpoint, &alias).unwrap();
        let original = options.mountpoint.clone();
        options.mountpoint = alias;
        assert!(options.validate().is_err());
        options.mountpoint = options.data.clone();
        assert!(options.validate().is_err());
        options.mountpoint = original;
        fs::remove_dir(&options.mountpoint).unwrap();
        assert!(options.validate().is_err());
    }

    #[test]
    fn fresh_namespace_uses_real_layer_budget_and_rejects_reopen() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        let layer = fresh_layer(&options).unwrap();
        let budget = layer.workspace_budget(AGENT).unwrap();
        assert_eq!(budget.limit_bytes, options.budget);
        assert_eq!(budget.used_bytes, 0);
        assert!(budget.explicitly_configured);
        drop(layer);
        assert!(fresh_layer(&options).is_err());
    }
}
