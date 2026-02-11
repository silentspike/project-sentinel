use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Configuration for Multi-LoRA serving.
pub struct MultiLoraConfig {
    /// Directory containing LoRA adapter files: {agent_name}.bin
    pub adapter_dir: PathBuf,
    /// Currently loaded adapter (None = base model only)
    pub current_adapter: Option<String>,
}

/// Manages LoRA adapters for 54 agent personalities.
///
/// Ein Basismodell, 54 individuelle LoRA-Adapter.
/// Auf CPU: Sequentielles Adapter-Swapping (ein Adapter gleichzeitig aktiv).
/// Auf GPU (spaeter): SGMV-Kernel fuer parallele Adapter via vLLM/SGLang.
pub struct LoraManager {
    config: MultiLoraConfig,
    /// Cache: adapter_name -> file_path (verifiziert dass Datei existiert)
    adapter_cache: HashMap<String, PathBuf>,
}

impl LoraManager {
    pub fn new(adapter_dir: PathBuf) -> Self {
        Self {
            config: MultiLoraConfig {
                adapter_dir: adapter_dir.clone(),
                current_adapter: None,
            },
            adapter_cache: HashMap::new(),
        }
    }

    /// Scannt adapter_dir und cached alle verfuegbaren Adapter.
    pub fn scan_adapters(&mut self) -> anyhow::Result<usize> {
        self.adapter_cache.clear();
        for entry in std::fs::read_dir(&self.config.adapter_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "bin").unwrap_or(false) {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    self.adapter_cache.insert(name.to_string(), path);
                }
            }
        }
        Ok(self.adapter_cache.len())
    }

    /// Gibt den Pfad zum LoRA-Adapter fuer einen Agent zurueck.
    pub fn get_adapter_path(&self, agent_name: &str) -> Option<&Path> {
        self.adapter_cache.get(agent_name).map(|p| p.as_path())
    }

    /// Prueft ob ein Adapter fuer diesen Agent existiert.
    pub fn has_adapter(&self, agent_name: &str) -> bool {
        self.adapter_cache.contains_key(agent_name)
    }

    /// Gibt alle verfuegbaren Adapter-Namen zurueck.
    pub fn available_adapters(&self) -> Vec<&str> {
        self.adapter_cache.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lora_manager_scan() {
        let tmp_dir = tempfile::tempdir().unwrap();
        std::fs::write(tmp_dir.path().join("thomas.bin"), b"dummy").unwrap();
        std::fs::write(tmp_dir.path().join("lisa.bin"), b"dummy").unwrap();
        std::fs::write(tmp_dir.path().join("readme.txt"), b"not an adapter").unwrap();

        let mut manager = LoraManager::new(tmp_dir.path().to_path_buf());
        let count = manager.scan_adapters().unwrap();
        assert_eq!(count, 2);
        assert!(manager.has_adapter("thomas"));
        assert!(manager.has_adapter("lisa"));
        assert!(!manager.has_adapter("readme"));
    }

    #[test]
    fn test_lora_manager_missing_adapter() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let manager = LoraManager::new(tmp_dir.path().to_path_buf());
        assert!(!manager.has_adapter("nonexistent"));
        assert!(manager.get_adapter_path("nonexistent").is_none());
    }
}
