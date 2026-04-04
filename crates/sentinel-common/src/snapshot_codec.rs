use anyhow::{anyhow, Context};

use crate::WorldSnapshot;

/// Bincode 2 in legacy mode keeps wire compatibility with the historic
/// `bincode::serialize` / `deserialize` snapshots from bincode 1.x.
fn legacy_config() -> impl bincode::config::Config {
    bincode::config::legacy()
}

pub fn encode_world_snapshot(snapshot: &WorldSnapshot) -> anyhow::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(snapshot, legacy_config())
        .context("World Snapshot serialisieren")
}

pub fn decode_world_snapshot(bytes: &[u8]) -> anyhow::Result<WorldSnapshot> {
    let (snapshot, consumed) = bincode::serde::decode_from_slice(bytes, legacy_config())
        .context("World Snapshot deserialisieren")?;
    if consumed != bytes.len() {
        return Err(anyhow!(
            "World Snapshot enthaelt {} ungenutzte Bytes",
            bytes.len() - consumed
        ));
    }
    Ok(snapshot)
}
