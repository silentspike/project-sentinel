//! Content-addressing the manifest-capable parts of a microVM snapshot (#500a).
//!
//! ## RAM-page boundary (honest scope, AC-5)
//!
//! A Firecracker full snapshot writes two *host files*: the VM state file
//! (`snapshot.state`) and the guest-RAM file (`snapshot.mem`). Both are ordinary
//! disk files, so they are content-addressable into the CAS and can travel as a
//! [`BlockRef`] instead of an inline byte copy. That is the manifest-capable part.
//!
//! What this deliberately does **not** do, and why:
//! - **Dedup of the RAM file is ~zero.** A multi-GB guest-RAM dump changes almost
//!   entirely between snapshots, so content-defined chunking of it buys nothing
//!   and costs a lot. The RAM file is therefore referenced as a single SHA-256
//!   whole-blob, not chunked.
//! - **The live guest-RAM pages are never captured here.** They are
//!   non-deterministic and are never serialized into the `NanoSnapshot` payload
//!   (only the file paths are). Deep microVM migration — post-copy of live pages
//!   and the consistency class — is Track F (#554), not #500a.
//!
//! So: a RAM page is not an ECS state is not a bwrap home. This module only proves
//! the file-content-addressing mechanism and makes no live-migration claim.
//! See `docs/microvm-ram-boundary.md`.

use std::path::Path;

use anyhow::{anyhow, Result};
use sentinel_common::{BlockRef, HashAlgorithm};
use sentinel_fs::cas::CasStore;

/// Content-address a microVM snapshot file (state or mem) into the CAS, returning
/// a whole-blob SHA-256 [`BlockRef`]. Chunk dedup is intentionally not used (see
/// the module docs: a RAM dump barely dedups).
pub fn content_address_file(cas: &CasStore, path: &Path) -> Result<BlockRef> {
    let bytes =
        std::fs::read(path).map_err(|e| anyhow!("read snapshot file {}: {e}", path.display()))?;
    let (hash, _deduped) = cas.store(&bytes)?;
    Ok(BlockRef::blob_sha256(hash, bytes.len() as u64))
}

/// Restore a previously content-addressed snapshot file from the CAS to `dest`.
pub fn restore_file(cas: &CasStore, block_ref: &BlockRef, dest: &Path) -> Result<()> {
    if block_ref.algorithm() != HashAlgorithm::Sha256 {
        return Err(anyhow!(
            "microVM snapshot file ref must be sha256, got {:?}",
            block_ref.algorithm()
        ));
    }
    let digest: [u8; 32] = block_ref
        .digest()
        .try_into()
        .map_err(|_| anyhow!("sha256 digest is not 32 bytes"))?;
    let bytes = cas.read(&digest)?;
    std::fs::write(dest, &bytes)
        .map_err(|e| anyhow!("write restored file {}: {e}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-5: the manifest-capable parts (the mem/state disk files) content-address
    // and round-trip through the CAS. Uses a SMALL synthetic file, not a real
    // multi-GB RAM dump (which would not dedup anyway — see module docs).
    #[test]
    fn ac5_snapshot_file_content_addresses_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::open(dir.path()).unwrap();

        let mem = dir.path().join("snapshot.mem");
        std::fs::write(
            &mem,
            b"small synthetic guest-ram-ish snapshot bytes (not a real dump)",
        )
        .unwrap();

        let bref = content_address_file(&cas, &mem).unwrap();
        assert_eq!(bref.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(bref.namespace(), sentinel_common::BlockNamespace::Blob);

        let restored = dir.path().join("restored.mem");
        restore_file(&cas, &bref, &restored).unwrap();
        assert_eq!(
            std::fs::read(&mem).unwrap(),
            std::fs::read(&restored).unwrap(),
            "mem file must round-trip byte-identically"
        );
    }

    #[test]
    fn restore_rejects_non_sha256_ref() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::open(dir.path()).unwrap();
        let bad = BlockRef::chunk_blake3_128([0u8; 16], 4, "gear-v1");
        assert!(restore_file(&cas, &bad, &dir.path().join("x")).is_err());
    }
}
