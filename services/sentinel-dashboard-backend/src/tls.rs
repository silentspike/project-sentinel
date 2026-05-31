//! Self-signed TLS-Cert (#431) — geteilt vom HTTPS-Server (axum) **und** dem WebTransport-Endpoint.
//!
//! TLS-Primaerpfad (Maintainer 2026-05-31): self-signed + Cert-Hash-Pinning über LAN-IP (wie #439).
//! WebTransport `serverCertificateHashes` verlangt ein Cert mit Gueltigkeit <14 Tage → not_after = +13d.
//! Der sha-256(DER)-Hash (base64) wird über `GET /api/cert-hash` ausgeliefert; der Browser pinnt ihn.
//! (Optionaler CA-Cert-Pfad = eigenes Infra-Folge-Issue, netbird/AD-CS.)

use std::path::{Path, PathBuf};

use base64::Engine;
use sha2::{Digest, Sha256};

/// Pfade + Hash des geteilten self-signed Certs.
pub struct SharedCert {
    pub cert_pem_path: PathBuf,
    pub key_pem_path: PathBuf,
    /// base64(sha-256(cert DER)) — fuer WebTransport `serverCertificateHashes`.
    pub cert_hash_b64: String,
}

/// Erzeugt ein self-signed Cert (Gueltigkeit 13 Tage) fuer die gegebenen SANs, schreibt cert+key als
/// PEM in `out_dir` (von axum-server `from_pem_file` + wtransport `load_pemfiles` gelesen).
pub fn generate(out_dir: &Path, sans: &[&str]) -> anyhow::Result<SharedCert> {
    std::fs::create_dir_all(out_dir)?;
    let params = {
        let mut p = rcgen::CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())?;
        p.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        p.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(13);
        p
    };
    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_der = cert.der().to_vec();
    let cert_hash_b64 = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&cert_der));

    let cert_pem_path = out_dir.join("console-cert.pem");
    let key_pem_path = out_dir.join("console-key.pem");
    std::fs::write(&cert_pem_path, cert.pem())?;
    std::fs::write(&key_pem_path, key_pair.serialize_pem())?;

    tracing::info!(?cert_pem_path, cert_hash_b64 = %cert_hash_b64, "self-signed console cert generated (13d)");
    Ok(SharedCert { cert_pem_path, key_pem_path, cert_hash_b64 })
}
