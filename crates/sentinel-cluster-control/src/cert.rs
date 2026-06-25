//! Self-signed node identity + cert-pinning fingerprints (V10).
//!
//! Track-A is a single trust domain (V21): there is no CA, no rotation and no
//! revocation (those are Track-D2 / G-D2). Each node holds a self-signed cert and
//! pins its peers' cert fingerprints; the QUIC layer accepts the self-signed cert
//! and the application enforces the pin post-handshake (`server`/`client`).

use std::path::Path;

use sha2::{Digest, Sha256};

/// A self-signed node identity (certificate + private key, both DER).
pub struct NodeCertificate {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl NodeCertificate {
    /// Generate a fresh self-signed cert with `node_alias` as the SAN.
    pub fn generate(node_alias: &str) -> anyhow::Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec![node_alias.to_string()])
            .map_err(|e| anyhow::anyhow!("generate self-signed cert for {node_alias}: {e}"))?;
        Ok(Self {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.key_pair.serialize_der(),
        })
    }

    /// Load the persisted cert+key, or generate + persist a fresh pair. The
    /// fingerprint MUST be stable across restarts (otherwise a peer's pin breaks on
    /// every reboot), so the node generates its identity once and reuses it. The
    /// private key is written `0600`.
    pub fn load_or_generate(
        cert_path: &Path,
        key_path: &Path,
        node_alias: &str,
    ) -> anyhow::Result<Self> {
        if cert_path.exists() && key_path.exists() {
            let cert_der = std::fs::read(cert_path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", cert_path.display()))?;
            let key_der = std::fs::read(key_path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", key_path.display()))?;
            return Ok(Self { cert_der, key_der });
        }
        let node = Self::generate(node_alias)?;
        node.persist(cert_path, key_path)?;
        Ok(node)
    }

    /// Write the cert (`0644`) + key (`0600`) to disk.
    pub fn persist(&self, cert_path: &Path, key_path: &Path) -> anyhow::Result<()> {
        std::fs::write(cert_path, &self.cert_der)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", cert_path.display()))?;
        std::fs::write(key_path, &self.key_der)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| anyhow::anyhow!("chmod 0600 {}: {e}", key_path.display()))?;
        }
        Ok(())
    }

    /// The SHA-256 fingerprint a peer pins against (V10).
    pub fn fingerprint(&self) -> CertFingerprint {
        CertFingerprint::of_der(&self.cert_der)
    }
}

/// SHA-256 fingerprint of a DER certificate — the V10 pin a peer matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CertFingerprint(pub [u8; 32]);

impl CertFingerprint {
    pub fn of_der(cert_der: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(cert_der);
        Self(h.finalize().into())
    }

    pub fn to_hex(&self) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(64);
        for b in self.0 {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Parse a 64-char lowercase/uppercase hex fingerprint (the pin a peer is
    /// configured with). Returns `None` on a malformed value.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl std::fmt::Display for CertFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_distinguishes_certs() {
        let a = NodeCertificate::generate("node-0").unwrap();
        let b = NodeCertificate::generate("node-1").unwrap();
        assert_eq!(a.fingerprint(), a.fingerprint(), "stable");
        assert_ne!(a.fingerprint(), b.fingerprint(), "distinct certs differ");
        assert_eq!(a.fingerprint().to_hex().len(), 64);
        assert_eq!(CertFingerprint::of_der(&a.cert_der), a.fingerprint());
    }

    #[test]
    fn generated_cert_and_key_are_nonempty_der() {
        let c = NodeCertificate::generate("node-x").unwrap();
        assert!(!c.cert_der.is_empty());
        assert!(!c.key_der.is_empty());
    }

    #[test]
    fn fingerprint_hex_roundtrips() {
        let fp = NodeCertificate::generate("n").unwrap().fingerprint();
        assert_eq!(CertFingerprint::from_hex(&fp.to_hex()), Some(fp));
        assert_eq!(
            CertFingerprint::from_hex(&format!(" {}\n", fp.to_hex())),
            Some(fp)
        );
        assert!(CertFingerprint::from_hex("tooshort").is_none());
        assert!(CertFingerprint::from_hex(&"z".repeat(64)).is_none());
    }

    #[test]
    fn load_or_generate_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("node-cert.der");
        let key = dir.path().join("node-key.der");
        let a = NodeCertificate::load_or_generate(&cert, &key, "node-0").unwrap();
        // Second call loads the persisted identity → same fingerprint (pins survive).
        let b = NodeCertificate::load_or_generate(&cert, &key, "node-0").unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private key must be 0600");
        }
    }
}
