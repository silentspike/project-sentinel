//! Sandbox-Konfiguration fuer Tool-Ausfuehrung.
//!
//! Definiert Resource-Limits und Filesystem-Einschraenkungen.
//! Native Tools (FileRead/FileWrite) werden gegen `allowed_paths` geprueft.
//! WASM-Module erhalten Limits via Engine-Konfiguration.

use crate::scoped_fs::ScopedPath;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Sandbox-Konfiguration fuer eine Tool-Ausfuehrung.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximale CPU-Zeit in Millisekunden.
    pub max_cpu_ms: u64,
    /// Maximaler Speicher in Bytes.
    pub max_memory_bytes: usize,
    /// Erlaubte Dateisystem-Pfade (leer = kein Zugriff).
    pub allowed_paths: Vec<PathBuf>,
    /// Maximale Ausfuehrungszeit (Wall-Clock).
    pub max_execution_time: Duration,
}

impl SandboxConfig {
    /// Restriktive Sandbox: kein Dateizugriff, 500ms CPU, 10MB RAM.
    pub fn restrictive() -> Self {
        Self {
            max_cpu_ms: 500,
            max_memory_bytes: 10 * 1024 * 1024,
            allowed_paths: Vec::new(),
            max_execution_time: Duration::from_millis(500),
        }
    }

    /// Sandbox mit spezifischen erlaubten Pfaden.
    pub fn with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            allowed_paths: paths,
            ..Self::restrictive()
        }
    }

    /// Prueft ob ein Pfad innerhalb der erlaubten Pfade liegt.
    ///
    /// Advisory only; execution must use the descriptor-bound scoped path.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        self.scoped_path(path)
            .is_ok_and(|path| path.exists_or_new_leaf())
    }

    pub(crate) fn scoped_path(&self, path: &Path) -> std::io::Result<ScopedPath> {
        let absolute = |path: &Path| {
            if path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                None
            } else {
                std::path::absolute(path).ok()
            }
        };
        if let Some(path) = absolute(path) {
            for allowed in &self.allowed_paths {
                let Some(root) = absolute(allowed) else {
                    continue;
                };
                if let Ok(relative) = path.strip_prefix(&root) {
                    if let Ok(scoped) = ScopedPath::new(&root, relative) {
                        return Ok(scoped);
                    }
                }
                // Preserve exact regular-file grants, without broadening to siblings.
                if path == root && root.symlink_metadata().is_ok_and(|m| m.is_file()) {
                    if let (Some(parent), Some(leaf)) = (root.parent(), root.file_name()) {
                        if let Ok(scoped) = ScopedPath::new(parent, Path::new(leaf)) {
                            return Ok(scoped.exact_leaf());
                        }
                    }
                }
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Sandbox violation: path is not within an allowed root",
        ))
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self::restrictive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn restrictive_blocks_all_paths() {
        let sandbox = SandboxConfig::restrictive();
        assert!(!sandbox.is_path_allowed(Path::new("/etc/passwd")));
        assert!(!sandbox.is_path_allowed(Path::new("/tmp/anything")));
    }

    #[test]
    fn allowed_path_permits_access() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "data").unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        assert!(sandbox.is_path_allowed(&file));
    }

    #[test]
    fn disallowed_path_blocked() {
        let allowed_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let other_file = other_dir.path().join("secret.txt");
        fs::write(&other_file, "secret").unwrap();

        let sandbox = SandboxConfig::with_paths(vec![allowed_dir.path().to_path_buf()]);
        assert!(!sandbox.is_path_allowed(&other_file));
    }

    #[test]
    fn new_file_in_allowed_dir_permitted() {
        let dir = tempfile::tempdir().unwrap();
        // Datei existiert noch nicht, aber Parent ist erlaubt
        let new_file = dir.path().join("new_file.txt");

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        assert!(sandbox.is_path_allowed(&new_file));
    }

    #[test]
    fn empty_paths_blocks_everything() {
        let sandbox = SandboxConfig {
            allowed_paths: Vec::new(),
            ..SandboxConfig::restrictive()
        };
        let dir = tempfile::tempdir().unwrap();
        assert!(!sandbox.is_path_allowed(dir.path()));
    }

    #[test]
    fn dangling_external_symlink_is_not_a_new_authorized_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(outside.path().join("missing"), &link).unwrap();
        let sandbox = SandboxConfig::with_paths(vec![root.path().to_path_buf()]);
        assert!(!sandbox.is_path_allowed(&link));
        assert!(sandbox
            .scoped_path(&link)
            .unwrap()
            .write(b"escape", false)
            .is_err());
        assert!(!outside.path().join("missing").exists());
    }

    #[test]
    fn exact_regular_file_grant_does_not_authorize_its_siblings() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("allowed");
        fs::write(&file, "inside").unwrap();
        fs::write(root.path().join("other"), "outside").unwrap();
        let sandbox = SandboxConfig::with_paths(vec![file.clone()]);
        assert!(sandbox.is_path_allowed(&file));
        assert_eq!(
            sandbox.scoped_path(&file).unwrap().read_text().unwrap(),
            "inside"
        );
        assert!(!sandbox.is_path_allowed(&root.path().join("other")));
    }

    #[test]
    fn exact_file_grant_rejects_late_symlink_to_an_ungranted_sibling() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("allowed");
        let other = root.path().join("other");
        fs::write(&file, "inside").unwrap();
        fs::write(&other, "unchanged").unwrap();
        let sandbox = SandboxConfig::with_paths(vec![file.clone()]);
        let scoped = sandbox.scoped_path(&file).unwrap();
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("other", &file).unwrap();
        assert!(scoped.read_bytes().is_err());
        assert!(scoped.write(b"escape", false).is_err());
        assert_eq!(fs::read_to_string(other).unwrap(), "unchanged");
    }
}
