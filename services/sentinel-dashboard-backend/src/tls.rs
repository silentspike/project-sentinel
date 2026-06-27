//! Dashboard TLS certificate handling (#431/#473), shared by the HTTPS server
//! and the WebTransport endpoint.
//!
//! Zero-Config mode generates a 13-day self-signed certificate and exposes its
//! SHA-256 hash through `GET /api/cert-hash` for WebTransport
//! `serverCertificateHashes` pinning.
//!
//! Production mode loads deployer-provided PEM files and disables pinning by
//! returning no certificate hash; the browser then uses normal CA validation.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use base64::Engine;
use sha2::{Digest, Sha256};

/// Paths plus optional hash for the certificate shared by HTTPS and WebTransport.
#[derive(Debug)]
pub struct SharedCert {
    pub cert_pem_path: PathBuf,
    pub key_pem_path: PathBuf,
    /// base64(sha-256(cert DER)) for WebTransport `serverCertificateHashes`.
    pub cert_hash_b64: Option<String>,
}

pub fn resolve_dashboard_cert(
    out_dir: &Path,
    sans: &[&str],
    tls_cert_path: Option<&str>,
    tls_key_path: Option<&str>,
) -> anyhow::Result<SharedCert> {
    match (tls_cert_path, tls_key_path) {
        (None, None) => generate(out_dir, sans),
        (Some(cert_path), Some(key_path)) => load_provided(cert_path, key_path),
        _ => {
            bail!("SENTINEL_DASHBOARD_TLS_CERT and SENTINEL_DASHBOARD_TLS_KEY must be set together")
        }
    }
}

pub fn load_provided(
    cert_pem_path: impl Into<PathBuf>,
    key_pem_path: impl Into<PathBuf>,
) -> anyhow::Result<SharedCert> {
    let cert_pem_path = cert_pem_path.into();
    let key_pem_path = key_pem_path.into();
    ensure_readable(&cert_pem_path, "SENTINEL_DASHBOARD_TLS_CERT")?;
    ensure_readable(&key_pem_path, "SENTINEL_DASHBOARD_TLS_KEY")?;
    tracing::info!(
        ?cert_pem_path,
        ?key_pem_path,
        "provided dashboard TLS certificate loaded; WebTransport certificate pinning disabled"
    );
    Ok(SharedCert {
        cert_pem_path,
        key_pem_path,
        cert_hash_b64: None,
    })
}

fn ensure_readable(path: &Path, env_name: &str) -> anyhow::Result<()> {
    if !path.is_file() {
        bail!(
            "{env_name} must point to a readable file: {}",
            path.display()
        );
    }
    std::fs::File::open(path).with_context(|| {
        format!(
            "{env_name} must point to a readable file: {}",
            path.display()
        )
    })?;
    Ok(())
}

/// Erzeugt ein self-signed Cert (Gueltigkeit 13 Tage) fuer die gegebenen SANs, schreibt cert+key als
/// PEM in `out_dir` (von axum-server `from_pem_file` + wtransport `load_pemfiles` gelesen).
pub fn generate(out_dir: &Path, sans: &[&str]) -> anyhow::Result<SharedCert> {
    std::fs::create_dir_all(out_dir)?;
    let params = {
        let mut p =
            rcgen::CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())?;
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
    Ok(SharedCert {
        cert_pem_path,
        key_pem_path,
        cert_hash_b64: Some(cert_hash_b64),
    })
}

#[cfg(test)]
mod tests {
    use super::{load_provided, resolve_dashboard_cert};

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_generates_self_signed_hash_without_tls_env() {
        let dir = temp_dir("dashboard-zero-config-cert");
        let cert = resolve_dashboard_cert(&dir, &["localhost", "127.0.0.1"], None, None).unwrap();
        assert!(cert.cert_pem_path.exists());
        assert!(cert.key_pem_path.exists());
        assert!(cert
            .cert_hash_b64
            .as_deref()
            .is_some_and(|hash| !hash.is_empty()));
    }

    #[test]
    fn load_provided_cert_disables_hash_pinning() {
        let dir = temp_dir("dashboard-provided-cert");
        let cert_path = dir.join("provided-cert.pem");
        let key_path = dir.join("provided-key.pem");
        std::fs::write(&cert_path, "cert").unwrap();
        std::fs::write(&key_path, "key").unwrap();

        let cert = load_provided(&cert_path, &key_path).unwrap();
        assert_eq!(cert.cert_pem_path, cert_path);
        assert_eq!(cert.key_pem_path, key_path);
        assert!(cert.cert_hash_b64.is_none());
    }

    #[test]
    fn resolve_rejects_partial_tls_env() {
        let dir = temp_dir("dashboard-partial-cert");
        let err = resolve_dashboard_cert(&dir, &["localhost"], Some("/missing/cert.pem"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be set together"));
    }

    #[test]
    fn load_provided_rejects_unreadable_paths() {
        let dir = temp_dir("dashboard-missing-cert");
        let cert_path = dir.join("missing-cert.pem");
        let key_path = dir.join("missing-key.pem");
        let err = load_provided(&cert_path, &key_path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("SENTINEL_DASHBOARD_TLS_CERT"));
        assert!(err.contains("readable file"));
    }
}
