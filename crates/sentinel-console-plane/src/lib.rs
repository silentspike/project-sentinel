//! CAS-Konsolen-Datenebene (#439, Epic #436).
//!
//! Overhead-/IO-minimaler Datenfluss Backend→Konsole auf Sentinels eigenem 1:n-Pointer/CAS-Prinzip:
//! Konsolen-Daten (Conversations/Telemetrie/Events) werden via content-defined chunking (FastCDC,
//! aus `sentinel-fs`) + blake3-128 content-addressed, **dedupliziert** (refcount) und zstd-komprimiert.
//! Statt 1s-Voll-State-Poll laeuft der Transport als **Manifest + Delta**: der Client schickt die
//! Block-Hashes, die er hat; der Server liefert nur die fehlenden Bloecke (Conversations/System-
//! Bloecke sind massiv redundant → Dedup greift stark). Auf Stream/Append optimiert (append-only
//! Block-Log), nicht der FUSE-Layer.

use std::collections::{HashMap, HashSet};

use sentinel_fs::chunker::{chunk_data, ChunkHash};

/// 128-bit content address eines Blocks (blake3-128, identisch zu sentinel-fs Chunk-Hash).
pub type BlockHash = ChunkHash;

/// Zstd-Kompressionslevel fuer gespeicherte/uebertragene Bloecke (wie sentinel-fs CAS).
const ZSTD_LEVEL: i32 = 3;

/// Append-only, dedupliziertes Block-Log der Konsolen-Daten.
#[derive(Default)]
pub struct ConsolePlane {
    /// hash → zstd-komprimierte Block-Daten (jeder eindeutige Block genau einmal).
    blocks: HashMap<BlockHash, Vec<u8>>,
    /// hash → Referenzzaehler (wie oft der Block im Log vorkommt).
    refcount: HashMap<BlockHash, u32>,
    /// Append-only Reihenfolge ALLER ingestierten Block-Hashes (inkl. Wiederholungen).
    log: Vec<BlockHash>,
    total_ingested_bytes: u64,
    stored_bytes: u64,
}

/// Server→Client Delta: fehlende Block-Hashes + ihre (komprimierten) Daten.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Delta {
    pub missing: Vec<BlockHash>,
    /// (hash, zstd-komprimierte Block-Daten) — nur fuer die fehlenden Bloecke.
    pub blocks: Vec<(BlockHash, Vec<u8>)>,
}

impl Delta {
    /// Summe der uebertragenen (komprimierten) Block-Bytes.
    pub fn transfer_bytes(&self) -> u64 {
        self.blocks.iter().map(|(_, b)| b.len() as u64).sum()
    }
}

// ── Wire-Format (msgpack/bincode-serialisierbar, vom Service genutzt) ──

/// Client→Server: Manifest der Block-Hashes, die der Client bereits hat.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct HelloManifest {
    pub have: Vec<BlockHash>,
}

/// Server→Client Push (Append): bei neuem Event die neuen Block-Hashes + fehlende Bloecke.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Push {
    pub event_hashes: Vec<BlockHash>,
    pub blocks: Vec<(BlockHash, Vec<u8>)>,
}

impl ConsolePlane {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingestiert Konsolen-Daten: CDC-Chunking + Dedup + zstd. Gibt die Block-Hashes des Eintrags
    /// in Reihenfolge zurueck (fuer einen Push an verbundene Clients).
    pub fn ingest(&mut self, data: &[u8]) -> Vec<BlockHash> {
        let mut hashes = Vec::new();
        for chunk in chunk_data(data) {
            let h = chunk.hash;
            self.total_ingested_bytes += chunk.data.len() as u64;
            *self.refcount.entry(h).or_insert(0) += 1;
            if !self.blocks.contains_key(&h) {
                let compressed = zstd::encode_all(chunk.data.as_slice(), ZSTD_LEVEL)
                    .unwrap_or_else(|_| chunk.data.clone());
                self.stored_bytes += compressed.len() as u64;
                self.blocks.insert(h, compressed);
            }
            self.log.push(h);
            hashes.push(h);
        }
        hashes
    }

    /// Komprimierte Block-Daten (fuer Push/Delta).
    pub fn block(&self, hash: &BlockHash) -> Option<&[u8]> {
        self.blocks.get(hash).map(|v| v.as_slice())
    }

    /// Dekomprimierte Block-Daten.
    pub fn read_block(&self, hash: &BlockHash) -> Option<Vec<u8>> {
        self.blocks
            .get(hash)
            .map(|c| zstd::decode_all(c.as_slice()).unwrap_or_else(|_| c.clone()))
    }

    /// Server-Manifest: alle eindeutigen Block-Hashes, die der Server vorhaelt.
    pub fn server_manifest(&self) -> Vec<BlockHash> {
        self.blocks.keys().copied().collect()
    }

    /// Berechnet das Delta: Bloecke, die der Server hat, der Client (laut Manifest) aber nicht.
    /// Genau das ersetzt den Voll-State-Poll — Client zieht nur die fehlenden ~10%.
    pub fn delta(&self, client_has: &HashSet<BlockHash>) -> Delta {
        let mut missing = Vec::new();
        let mut blocks = Vec::new();
        for (hash, compressed) in &self.blocks {
            if !client_has.contains(hash) {
                missing.push(*hash);
                blocks.push((*hash, compressed.clone()));
            }
        }
        Delta { missing, blocks }
    }

    /// Baut einen Push fuer einen frisch ingestierten Eintrag: nur die Bloecke, die der Client
    /// noch nicht hat (per `client_has`), plus die vollstaendige Hash-Liste des Eintrags.
    pub fn push_for(&self, event_hashes: &[BlockHash], client_has: &HashSet<BlockHash>) -> Push {
        let mut seen = HashSet::new();
        let mut blocks = Vec::new();
        for h in event_hashes {
            if !client_has.contains(h) && seen.insert(*h) {
                if let Some(c) = self.blocks.get(h) {
                    blocks.push((*h, c.clone()));
                }
            }
        }
        Push {
            event_hashes: event_hashes.to_vec(),
            blocks,
        }
    }

    /// Anzahl eindeutiger Bloecke (dedupliziert).
    pub fn unique_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Anzahl ingestierter Bloecke gesamt (inkl. Wiederholungen).
    pub fn total_blocks(&self) -> usize {
        self.log.len()
    }

    /// Dedup-Ratio nach Blockzahl: 1 − unique/total (0 = keine Wiederholung, →1 = stark redundant).
    pub fn dedup_ratio(&self) -> f64 {
        if self.log.is_empty() {
            return 0.0;
        }
        1.0 - (self.blocks.len() as f64 / self.log.len() as f64)
    }

    /// Byte-Ersparnis: 1 − gespeicherte(komprimiert+dedupliziert) / ingestierte(roh).
    pub fn savings_ratio(&self) -> f64 {
        if self.total_ingested_bytes == 0 {
            return 0.0;
        }
        1.0 - (self.stored_bytes as f64 / self.total_ingested_bytes as f64)
    }

    pub fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }

    pub fn total_ingested_bytes(&self) -> u64 {
        self.total_ingested_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministisch-variierter Inhalt (nicht nur dasselbe Byte → CDC findet content-defined Grenzen,
    /// sodass der geteilte Block in mehrere wiederkehrende Chunks zerfaellt).
    fn varied(len: usize, salt: u32) -> Vec<u8> {
        (0..len as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(salt) >> 13) as u8)
            .collect()
    }

    /// Eine "Nachricht": grosser geteilter System-Block (identisch ueber alle Agents, salt=0) +
    /// kleiner eindeutiger Schwanz (Perception). Modelliert die reale Konsolen-Redundanz.
    fn message(unique_tail: usize) -> Vec<u8> {
        let mut m = varied(256 * 1024, 0); // 256 KB geteilter System-/company-context-Block
        m.extend(varied(4096, unique_tail as u32 + 1)); // eindeutiger Tail
        m
    }

    #[test]
    fn dedup_collapses_recurring_system_blocks() {
        // #439 AC-1: wiederkehrende System-Bloecke werden content-addressed dedupliziert (refcount).
        let mut plane = ConsolePlane::new();
        for i in 0..50 {
            plane.ingest(&message(i));
        }
        assert!(
            plane.total_blocks() > plane.unique_blocks(),
            "Wiederholungen existieren"
        );
        // Der grosse geteilte Block zerfaellt in mehrere Chunks, die nur EINMAL gespeichert werden
        // → hohe Dedup-Ratio + Byte-Ersparnis (Dedup, zstal auf variiertem Inhalt schwach).
        assert!(
            plane.dedup_ratio() > 0.5,
            "dedup ratio {} sollte > 0.5 sein (geteilte System-Bloecke)",
            plane.dedup_ratio()
        );
        assert!(
            plane.savings_ratio() > 0.5,
            "savings {} sollte > 0.5 sein (Dedup auf wiederkehrenden Bloecken)",
            plane.savings_ratio()
        );
    }

    #[test]
    fn delta_sends_only_missing_blocks() {
        // #439 AC-2/AC-3: Client mit ~90% bekannten Bloecken erhaelt nur die fehlenden ~10% Bytes.
        let mut plane = ConsolePlane::new();
        for i in 0..20 {
            plane.ingest(&message(i));
        }
        let full = plane.server_manifest();
        let full_bytes: u64 = full
            .iter()
            .map(|h| plane.block(h).map(|b| b.len() as u64).unwrap_or(0))
            .sum();

        // Client hat schon alles ausser den Bloecken eines neuen Eintrags.
        let new_hashes = plane.ingest(&message(999));
        let mut client_has: HashSet<BlockHash> = full.into_iter().collect();
        // Client kennt den (bereits vorhandenen) geteilten System-Block, nur der neue Tail fehlt.
        for h in &new_hashes {
            if plane.refcount.get(h).copied().unwrap_or(0) > 1 {
                client_has.insert(*h); // geteilter Block schon bekannt
            }
        }
        let delta = plane.delta(&client_has);
        assert!(
            !delta.missing.is_empty(),
            "es fehlt der neue, eindeutige Block"
        );
        assert!(
            delta.transfer_bytes() * 5 < full_bytes,
            "Delta {} muss deutlich kleiner als Voll-State {} sein",
            delta.transfer_bytes(),
            full_bytes
        );
    }

    #[test]
    fn block_roundtrip_compresses_and_restores() {
        let mut plane = ConsolePlane::new();
        let data = message(7);
        let hashes = plane.ingest(&data);
        // Rekonstruktion aus den Bloecken ergibt die Originaldaten.
        let mut restored = Vec::new();
        for h in &hashes {
            restored.extend(plane.read_block(h).expect("block present"));
        }
        assert_eq!(
            restored, data,
            "Block-Reassembly muss die Originaldaten ergeben"
        );
    }

    #[test]
    fn delta_empty_when_client_has_everything() {
        let mut plane = ConsolePlane::new();
        plane.ingest(&message(1));
        let client_has: HashSet<BlockHash> = plane.server_manifest().into_iter().collect();
        assert!(plane.delta(&client_has).missing.is_empty());
    }
}
