//! Sandbox-Konfiguration fuer Tool-Ausfuehrung.
//!
//! Definiert Resource-Limits und Filesystem-Einschraenkungen.
//! Native Tools (FileRead/FileWrite) werden gegen `allowed_paths` geprueft.
//! WASM-Module erhalten Limits via Engine-Konfiguration.

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
    /// Fuer existierende Pfade wird `canonicalize()` genutzt (folgt Symlinks).
    /// Fuer neue Dateien (Schreibzugriff) wird das Parent-Verzeichnis geprueft.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        if self.allowed_paths.is_empty() {
            return false;
        }

        // Versuche den Pfad zu kanonisieren
        let check_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // Pfad existiert nicht — prüfe ob Parent erlaubt ist (fuer Schreibzugriff)
                match path.parent().and_then(|p| p.canonicalize().ok()) {
                    Some(parent) => parent,
                    None => return false,
                }
            }
        };

        self.allowed_paths.iter().any(|allowed| {
            allowed
                .canonicalize()
                .map(|canonical| check_path.starts_with(&canonical))
                .unwrap_or(false)
        })
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
}
