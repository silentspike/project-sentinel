//! Crate-local backup support for Gaia Console Memory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BACKUP_FORMAT_VERSION, GRAPH_FILE_NAME, MEMORY_FILE_NAME};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaiaConsoleMemoryBackupFile {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaiaConsoleMemoryBackupBundle {
    pub format_version: u32,
    pub exported_at_ms: u64,
    pub graph_redb: GaiaConsoleMemoryBackupFile,
    pub memory_markdown: GaiaConsoleMemoryBackupFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaiaConsoleMemoryBackupIoReport {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaiaConsoleMemoryRestoreReport {
    pub data_dir: PathBuf,
    pub graph_redb: GaiaConsoleMemoryBackupIoReport,
    pub memory_markdown: GaiaConsoleMemoryBackupIoReport,
}

pub fn export_from_data_dir(
    data_dir: impl AsRef<Path>,
    exported_at_ms: u64,
) -> anyhow::Result<GaiaConsoleMemoryBackupBundle> {
    let data_dir = data_dir.as_ref();
    Ok(GaiaConsoleMemoryBackupBundle {
        format_version: BACKUP_FORMAT_VERSION,
        exported_at_ms,
        graph_redb: read_backup_file(data_dir.join(GRAPH_FILE_NAME), GRAPH_FILE_NAME)?,
        memory_markdown: read_backup_file(data_dir.join(MEMORY_FILE_NAME), MEMORY_FILE_NAME)?,
    })
}

pub fn write_bundle_to_path(
    bundle: &GaiaConsoleMemoryBackupBundle,
    path: impl AsRef<Path>,
    overwrite: bool,
) -> anyhow::Result<GaiaConsoleMemoryBackupIoReport> {
    validate_bundle(bundle)?;
    let path = path.as_ref();
    if path.exists() && !overwrite {
        bail!(
            "refusing to overwrite existing Gaia Console Memory backup {}",
            path.display()
        );
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create backup parent directory {}", parent.display()))?;
    }

    let bytes = bincode::serde::encode_to_vec(bundle, bincode::config::standard())
        .context("encode Gaia Console Memory backup bundle")?;
    write_atomic(path, &bytes, overwrite)?;
    Ok(GaiaConsoleMemoryBackupIoReport {
        path: path.to_path_buf(),
        size_bytes: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
    })
}

pub fn read_bundle_from_path(
    path: impl AsRef<Path>,
) -> anyhow::Result<GaiaConsoleMemoryBackupBundle> {
    let path = path.as_ref();
    let bytes = fs::read(path)
        .with_context(|| format!("read Gaia Console Memory backup {}", path.display()))?;
    let (bundle, decoded_len) =
        bincode::serde::decode_from_slice::<GaiaConsoleMemoryBackupBundle, _>(
            &bytes,
            bincode::config::standard(),
        )
        .context("decode Gaia Console Memory backup bundle")?;
    if decoded_len != bytes.len() {
        bail!(
            "Gaia Console Memory backup {} has trailing bytes",
            path.display()
        );
    }
    validate_bundle(&bundle)?;
    Ok(bundle)
}

pub fn restore_to_data_dir(
    data_dir: impl AsRef<Path>,
    bundle: &GaiaConsoleMemoryBackupBundle,
    overwrite: bool,
) -> anyhow::Result<GaiaConsoleMemoryRestoreReport> {
    validate_bundle(bundle)?;
    let data_dir = data_dir.as_ref();
    fs::create_dir_all(data_dir).with_context(|| {
        format!(
            "create Gaia Console Memory restore directory {}",
            data_dir.display()
        )
    })?;

    let graph_path = data_dir.join(GRAPH_FILE_NAME);
    let memory_path = data_dir.join(MEMORY_FILE_NAME);
    write_atomic(&graph_path, &bundle.graph_redb.bytes, overwrite)?;
    write_atomic(&memory_path, &bundle.memory_markdown.bytes, overwrite)?;

    Ok(GaiaConsoleMemoryRestoreReport {
        data_dir: data_dir.to_path_buf(),
        graph_redb: report_file(graph_path)?,
        memory_markdown: report_file(memory_path)?,
    })
}

fn read_backup_file(
    path: PathBuf,
    expected_name: &str,
) -> anyhow::Result<GaiaConsoleMemoryBackupFile> {
    let bytes = fs::read(&path)
        .with_context(|| format!("read Gaia Console Memory backup source {}", path.display()))?;
    Ok(GaiaConsoleMemoryBackupFile {
        file_name: expected_name.to_string(),
        size_bytes: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
        bytes,
    })
}

fn validate_bundle(bundle: &GaiaConsoleMemoryBackupBundle) -> anyhow::Result<()> {
    if bundle.format_version != BACKUP_FORMAT_VERSION {
        bail!(
            "unsupported Gaia Console Memory backup format {}",
            bundle.format_version
        );
    }
    validate_file(&bundle.graph_redb, GRAPH_FILE_NAME)?;
    validate_file(&bundle.memory_markdown, MEMORY_FILE_NAME)?;
    Ok(())
}

fn validate_file(file: &GaiaConsoleMemoryBackupFile, expected_name: &str) -> anyhow::Result<()> {
    if file.file_name != expected_name {
        bail!(
            "Gaia Console Memory backup file name mismatch: expected {}, got {}",
            expected_name,
            file.file_name
        );
    }
    if file.size_bytes != file.bytes.len() as u64 {
        bail!(
            "Gaia Console Memory backup {} size mismatch: metadata {}, actual {}",
            expected_name,
            file.size_bytes,
            file.bytes.len()
        );
    }
    let actual_sha = sha256_hex(&file.bytes);
    if file.sha256 != actual_sha {
        bail!(
            "Gaia Console Memory backup {} sha256 mismatch: expected {}, got {}",
            expected_name,
            file.sha256,
            actual_sha
        );
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8], overwrite: bool) -> anyhow::Result<()> {
    if path.exists() && !overwrite {
        bail!(
            "refusing to overwrite existing Gaia Console Memory file {}",
            path.display()
        );
    }
    let tmp = temp_path_for(path)?;
    fs::write(&tmp, bytes).with_context(|| format!("write temp file {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("restore Gaia Console Memory file {}", path.display()))?;
    Ok(())
}

fn temp_path_for(path: &Path) -> anyhow::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("restore path {} has no file name", path.display()))?;
    Ok(path.with_file_name(format!(".{file_name}.restore-tmp")))
}

fn report_file(path: PathBuf) -> anyhow::Result<GaiaConsoleMemoryBackupIoReport> {
    let bytes = fs::read(&path)
        .with_context(|| format!("read restored Gaia Console Memory file {}", path.display()))?;
    Ok(GaiaConsoleMemoryBackupIoReport {
        path,
        size_bytes: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{FactQuery, FactWrite, GaiaConsoleMemoryStore};
    use crate::memory_file::{GaiaConsoleMemoryFile, MemorySection};

    fn seed_data_dir(path: &Path) {
        let store = GaiaConsoleMemoryStore::open(path.join(GRAPH_FILE_NAME)).unwrap();
        store
            .insert_fact(FactWrite::literal(
                "company:sentinel",
                "memory_mode",
                "crate-local",
                1_000,
                1_000,
            ))
            .unwrap();
        let file = GaiaConsoleMemoryFile::open_or_create(path).unwrap();
        file.append_entry(
            MemorySection::SetupDecisions,
            1_000,
            "backup is separate from simulation snapshots",
        )
        .unwrap();
    }

    #[test]
    fn backup_roundtrip_restores_graph_and_markdown() {
        let source = tempfile::tempdir().unwrap();
        seed_data_dir(source.path());

        let bundle = export_from_data_dir(source.path(), 2_000).unwrap();
        assert_eq!(bundle.format_version, BACKUP_FORMAT_VERSION);
        assert_eq!(bundle.graph_redb.file_name, GRAPH_FILE_NAME);
        assert_eq!(bundle.memory_markdown.file_name, MEMORY_FILE_NAME);

        let bundle_path = source.path().join("gaia-memory.backup");
        let written = write_bundle_to_path(&bundle, &bundle_path, false).unwrap();
        assert!(written.size_bytes > 0);
        let loaded = read_bundle_from_path(&bundle_path).unwrap();

        let restored = tempfile::tempdir().unwrap();
        restore_to_data_dir(restored.path(), &loaded, false).unwrap();

        let store = GaiaConsoleMemoryStore::open(restored.path().join(GRAPH_FILE_NAME)).unwrap();
        let facts = store
            .query_facts(FactQuery::current("company:sentinel", "memory_mode"))
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact.valid_from_ms, 1_000);

        let markdown = fs::read_to_string(restored.path().join(MEMORY_FILE_NAME)).unwrap();
        assert!(markdown.contains("backup is separate from simulation snapshots"));
    }

    #[test]
    fn backup_restore_refuses_existing_files_without_overwrite() {
        let source = tempfile::tempdir().unwrap();
        seed_data_dir(source.path());
        let bundle = export_from_data_dir(source.path(), 2_000).unwrap();

        let restored = tempfile::tempdir().unwrap();
        seed_data_dir(restored.path());

        let error = restore_to_data_dir(restored.path(), &bundle, false).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));

        restore_to_data_dir(restored.path(), &bundle, true).unwrap();
    }

    #[test]
    fn backup_rejects_tampered_payload() {
        let source = tempfile::tempdir().unwrap();
        seed_data_dir(source.path());
        let mut bundle = export_from_data_dir(source.path(), 2_000).unwrap();
        bundle.memory_markdown.bytes[0] ^= 0xff;

        let error = restore_to_data_dir(source.path(), &bundle, true).unwrap_err();
        assert!(error.to_string().contains("sha256 mismatch"));
    }
}
