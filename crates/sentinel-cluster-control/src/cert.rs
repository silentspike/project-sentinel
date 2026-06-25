//! Self-signed node identity + cert-pinning fingerprints (V10).
//!
//! Track-A is a single trust domain (V21): there is no CA, no rotation and no
//! revocation (those are Track-D2 / G-D2). Each node holds a self-signed cert and
//! pins its peers' cert fingerprints; the QUIC layer accepts the self-signed cert
//! and the application enforces the pin post-handshake (`server`/`client`).

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
}
