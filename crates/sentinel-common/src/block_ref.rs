//! Content-addressed block reference (`BlockRef`) — the G2 / ADR-0498 hash model.
//!
//! A `BlockRef` is a self-describing, namespaced reference to a content-addressed
//! block. It exists so the two local hash worlds — SHA-256 whole blobs (`CasStore`)
//! and BLAKE3-128 content-defined chunks (`ArtifactPlane`) — can be referenced
//! unambiguously across crates and (later) across nodes (#498), without either
//! side guessing the hash space.
//!
//! Canonical, lossless serialization is **serde** (bincode/JSON). The
//! [`std::fmt::Display`] / [`std::str::FromStr`] string form
//! (`` `cas-blob:v1:sha256:<hex>` ``, `` `artifact-chunk:v1:blake3-128:gear-v1:<hex>` ``)
//! is a human-readable **locator only**: it does not encode `size_bytes`, so it is
//! intentionally lossy and must not be used where a round-trip is required.
//!
//! Trust boundary (ADR-0498): BLAKE3-128 is a trusted-cluster dedup identity, **not**
//! an adversarial security boundary. A transport-verified remote content id uses
//! SHA-256 / BLAKE3-256.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Which content-addressed namespace a [`BlockRef`] points into.
///
/// The namespace prevents accidentally resolving, say, a trash-queue digest as a
/// live blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockNamespace {
    /// A whole object stored in `CasStore` (SHA-256 keyed).
    Blob,
    /// A content-defined chunk stored in the `ArtifactPlane` (BLAKE3-128 keyed).
    Chunk,
    /// A digest pending deletion in a trash queue.
    FsTrash,
    /// A manifest object (a list of other refs).
    Manifest,
}

impl BlockNamespace {
    /// The label used in the human-readable [`Display`](fmt::Display) form.
    pub const fn wire_label(self) -> &'static str {
        match self {
            BlockNamespace::Blob => "cas-blob",
            BlockNamespace::Chunk => "artifact-chunk",
            BlockNamespace::FsTrash => "fs-trash",
            BlockNamespace::Manifest => "manifest",
        }
    }

    fn from_wire_label(s: &str) -> Option<Self> {
        match s {
            "cas-blob" => Some(BlockNamespace::Blob),
            "artifact-chunk" => Some(BlockNamespace::Chunk),
            "fs-trash" => Some(BlockNamespace::FsTrash),
            "manifest" => Some(BlockNamespace::Manifest),
            _ => None,
        }
    }
}

/// The digest algorithm of a [`BlockRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgorithm {
    /// SHA-256, 32-byte digest (whole-blob / object integrity identity).
    Sha256,
    /// BLAKE3 truncated to 128 bits, 16-byte digest (chunk dedup identity).
    Blake3_128,
    /// BLAKE3, 32-byte digest (transport-verified content identity).
    Blake3_256,
}

impl HashAlgorithm {
    /// Expected digest length in bytes for this algorithm.
    pub const fn digest_len(self) -> usize {
        match self {
            HashAlgorithm::Sha256 | HashAlgorithm::Blake3_256 => 32,
            HashAlgorithm::Blake3_128 => 16,
        }
    }

    /// The label used in the human-readable [`Display`](fmt::Display) form.
    pub const fn wire_label(self) -> &'static str {
        match self {
            HashAlgorithm::Sha256 => "sha256",
            HashAlgorithm::Blake3_128 => "blake3-128",
            HashAlgorithm::Blake3_256 => "blake3-256",
        }
    }

    fn from_wire_label(s: &str) -> Option<Self> {
        match s {
            "sha256" => Some(HashAlgorithm::Sha256),
            "blake3-128" => Some(HashAlgorithm::Blake3_128),
            "blake3-256" => Some(HashAlgorithm::Blake3_256),
            _ => None,
        }
    }
}

/// Errors constructing or parsing a [`BlockRef`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BlockRefError {
    /// The digest length did not match the declared algorithm.
    #[error("digest length {actual} does not match algorithm {algorithm:?} (expected {expected})")]
    DigestLength {
        /// The declared algorithm.
        algorithm: HashAlgorithm,
        /// The length the algorithm requires.
        expected: usize,
        /// The length that was supplied.
        actual: usize,
    },
    /// The string locator form could not be parsed.
    #[error("malformed BlockRef locator: {0}")]
    Parse(String),
}

/// A self-describing reference to a content-addressed block (ADR-0498, G2).
///
/// Fields are private so the digest-length invariant (a `Sha256` ref with a
/// 16-byte digest is unrepresentable) cannot be bypassed by struct-literal
/// construction; build via [`BlockRef::new`], [`BlockRef::chunk_blake3_128`] or
/// [`BlockRef::blob_sha256`]. Deserialization re-checks the invariant via
/// `#[serde(try_from)]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BlockRefRepr")]
pub struct BlockRef {
    namespace: BlockNamespace,
    algorithm: HashAlgorithm,
    digest: Vec<u8>,
    size_bytes: u64,
    chunk_profile: Option<String>,
    version: u16,
}

/// Serde mirror used only for deserialization so the digest-length invariant is
/// re-validated on untrusted input.
#[derive(Deserialize)]
struct BlockRefRepr {
    namespace: BlockNamespace,
    algorithm: HashAlgorithm,
    digest: Vec<u8>,
    size_bytes: u64,
    #[serde(default)]
    chunk_profile: Option<String>,
    version: u16,
}

impl TryFrom<BlockRefRepr> for BlockRef {
    type Error = BlockRefError;

    fn try_from(r: BlockRefRepr) -> Result<Self, Self::Error> {
        BlockRef::new(
            r.namespace,
            r.algorithm,
            r.digest,
            r.size_bytes,
            r.chunk_profile,
            r.version,
        )
    }
}

impl BlockRef {
    /// Construct a validated `BlockRef`. Returns [`BlockRefError::DigestLength`]
    /// if `digest.len()` does not match `algorithm`.
    pub fn new(
        namespace: BlockNamespace,
        algorithm: HashAlgorithm,
        digest: Vec<u8>,
        size_bytes: u64,
        chunk_profile: Option<String>,
        version: u16,
    ) -> Result<Self, BlockRefError> {
        let expected = algorithm.digest_len();
        if digest.len() != expected {
            return Err(BlockRefError::DigestLength {
                algorithm,
                expected,
                actual: digest.len(),
            });
        }
        Ok(Self {
            namespace,
            algorithm,
            digest,
            size_bytes,
            chunk_profile,
            version,
        })
    }

    /// Reference a content-defined chunk by its BLAKE3-128 hash (the canonical
    /// chunk identity used by the `ArtifactPlane`). `chunk_profile` records the
    /// chunker boundary settings so the walk is reproducible.
    pub fn chunk_blake3_128(
        hash: [u8; 16],
        size_bytes: u64,
        chunk_profile: impl Into<String>,
    ) -> Self {
        // The fixed-size array guarantees the digest-length invariant.
        Self {
            namespace: BlockNamespace::Chunk,
            algorithm: HashAlgorithm::Blake3_128,
            digest: hash.to_vec(),
            size_bytes,
            chunk_profile: Some(chunk_profile.into()),
            version: 1,
        }
    }

    /// Reference a whole blob by its SHA-256 hash (the `CasStore` identity).
    pub fn blob_sha256(hash: [u8; 32], size_bytes: u64) -> Self {
        Self {
            namespace: BlockNamespace::Blob,
            algorithm: HashAlgorithm::Sha256,
            digest: hash.to_vec(),
            size_bytes,
            chunk_profile: None,
            version: 1,
        }
    }

    /// The namespace this ref points into.
    pub fn namespace(&self) -> BlockNamespace {
        self.namespace
    }

    /// The digest algorithm.
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// The raw digest bytes (length matches [`HashAlgorithm::digest_len`]).
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }

    /// The size in bytes of the referenced content.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The chunk boundary profile, if this ref names a content-defined chunk.
    pub fn chunk_profile(&self) -> Option<&str> {
        self.chunk_profile.as_deref()
    }

    /// The ref schema version.
    pub fn version(&self) -> u16 {
        self.version
    }
}

impl fmt::Display for BlockRef {
    /// Human-readable locator (lossy: omits `size_bytes`). See the module docs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:v{}:{}:",
            self.namespace.wire_label(),
            self.version,
            self.algorithm.wire_label()
        )?;
        if let Some(profile) = &self.chunk_profile {
            write!(f, "{profile}:")?;
        }
        for byte in &self.digest {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for BlockRef {
    type Err = BlockRefError;

    /// Parse the lossy locator form. `size_bytes` cannot be recovered from the
    /// string and is set to `0`; use serde for a lossless round-trip.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        // ns : vN : algo : [profile (may itself contain ':') :] hex
        // The hex digest is always the final segment; the profile, if present,
        // is everything between the algorithm and the digest, so it may itself
        // contain ':' (e.g. "gear-v1:16k-64k-256k").
        if parts.len() < 4 {
            return Err(BlockRefError::Parse(format!(
                "expected at least 4 ':' segments, got {}",
                parts.len()
            )));
        }
        let namespace = BlockNamespace::from_wire_label(parts[0])
            .ok_or_else(|| BlockRefError::Parse(format!("unknown namespace '{}'", parts[0])))?;
        let version: u16 = parts[1]
            .strip_prefix('v')
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| BlockRefError::Parse(format!("bad version segment '{}'", parts[1])))?;
        let algorithm = HashAlgorithm::from_wire_label(parts[2])
            .ok_or_else(|| BlockRefError::Parse(format!("unknown algorithm '{}'", parts[2])))?;
        let last = parts.len() - 1;
        let hex = parts[last];
        let chunk_profile = if last > 3 {
            Some(parts[3..last].join(":"))
        } else {
            None
        };
        let digest = decode_hex(hex)
            .ok_or_else(|| BlockRefError::Parse(format!("invalid hex digest '{hex}'")))?;
        BlockRef::new(namespace, algorithm, digest, 0, chunk_profile, version)
    }
}

/// Decode a lowercase/uppercase hex string into bytes. Returns `None` on odd
/// length or a non-hex character.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_constructor_is_valid() {
        let r = BlockRef::chunk_blake3_128([7u8; 16], 4096, "gear-v1:16k-64k-256k");
        assert_eq!(r.namespace(), BlockNamespace::Chunk);
        assert_eq!(r.algorithm(), HashAlgorithm::Blake3_128);
        assert_eq!(r.digest().len(), 16);
        assert_eq!(r.size_bytes(), 4096);
        assert_eq!(r.chunk_profile(), Some("gear-v1:16k-64k-256k"));
    }

    #[test]
    fn blob_constructor_is_valid() {
        let r = BlockRef::blob_sha256([0xab; 32], 1234);
        assert_eq!(r.namespace(), BlockNamespace::Blob);
        assert_eq!(r.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(r.digest().len(), 32);
    }

    // AC-G: digest length must match the algorithm.
    #[test]
    fn new_rejects_wrong_digest_length() {
        // Sha256 with a 16-byte digest is unrepresentable.
        let err = BlockRef::new(
            BlockNamespace::Blob,
            HashAlgorithm::Sha256,
            vec![0u8; 16],
            10,
            None,
            1,
        )
        .unwrap_err();
        assert!(matches!(err, BlockRefError::DigestLength { expected: 32, actual: 16, .. }));

        // Blake3_128 with a 32-byte digest is also rejected.
        assert!(BlockRef::new(
            BlockNamespace::Chunk,
            HashAlgorithm::Blake3_128,
            vec![0u8; 32],
            10,
            Some("gear-v1".into()),
            1,
        )
        .is_err());
    }

    // AC-G: deserialization re-validates the invariant.
    #[test]
    fn deserialize_rejects_wrong_digest_length() {
        // 16-byte digest declared as sha256 -> must fail to deserialize.
        let bad = r#"{"namespace":"blob","algorithm":"sha256","digest":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"size_bytes":10,"chunk_profile":null,"version":1}"#;
        let parsed: Result<BlockRef, _> = serde_json::from_str(bad);
        assert!(parsed.is_err(), "wrong-length digest must not deserialize");
    }

    // AC-F: serde is the canonical, lossless round-trip form.
    #[test]
    fn serde_json_roundtrip_is_lossless() {
        let r = BlockRef::chunk_blake3_128([42u8; 16], 9001, "gear-v1:16k-64k-256k");
        let json = serde_json::to_string(&r).unwrap();
        let back: BlockRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn serde_bincode_roundtrip_is_lossless() {
        let r = BlockRef::blob_sha256([9u8; 32], 555);
        let bytes = bincode::serde::encode_to_vec(&r, bincode::config::standard()).unwrap();
        let (back, _): (BlockRef, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        assert_eq!(r, back);
    }

    // AC-F: the Display locator is lossy (size_bytes not encoded), so a
    // Display -> FromStr cycle is NOT guaranteed to equal the original.
    #[test]
    fn display_locator_format_matches_adr() {
        let blob = BlockRef::blob_sha256([0u8; 32], 10);
        assert_eq!(
            blob.to_string(),
            "cas-blob:v1:sha256:0000000000000000000000000000000000000000000000000000000000000000"
        );
        let chunk = BlockRef::chunk_blake3_128([0xff; 16], 10, "gear-v1");
        assert_eq!(
            chunk.to_string(),
            "artifact-chunk:v1:blake3-128:gear-v1:ffffffffffffffffffffffffffffffff"
        );
    }

    #[test]
    fn fromstr_parses_locator_but_loses_size() {
        let chunk = BlockRef::chunk_blake3_128([0x3c; 16], 4096, "gear-v1:16k-64k-256k");
        let parsed: BlockRef = chunk.to_string().parse().unwrap();
        // namespace/algorithm/digest/profile recovered...
        assert_eq!(parsed.namespace(), chunk.namespace());
        assert_eq!(parsed.algorithm(), chunk.algorithm());
        assert_eq!(parsed.digest(), chunk.digest());
        assert_eq!(parsed.chunk_profile(), chunk.chunk_profile());
        // ...but size is lost (lossy locator), so it is NOT equal.
        assert_eq!(parsed.size_bytes(), 0);
        assert_ne!(parsed, chunk);
    }

    #[test]
    fn fromstr_rejects_unknown_namespace_and_algorithm() {
        assert!("bogus-ns:v1:sha256:00".parse::<BlockRef>().is_err());
        assert!("cas-blob:v1:md5:00".parse::<BlockRef>().is_err());
        // hex that does not match the algorithm length is rejected by `new`.
        assert!("cas-blob:v1:sha256:00".parse::<BlockRef>().is_err());
    }
}
