//! QUIC TLS config for the control stream — self-signed certs with **app-level
//! fingerprint pinning** (V10), single trust domain (V21).
//!
//! There is no CA. The rustls verifier accepts any peer cert *identity* but still
//! **verifies the handshake signature** (so the peer proves it owns the presented
//! cert's private key — a replayed public cert cannot impersonate). Trust is then
//! decided by the application, which compares the peer's cert fingerprint against the
//! pinned set after the handshake (`server`/`client`). 0-RTT is off (ADR-2 / V18).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::cert::{CertFingerprint, NodeCertificate};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts any peer cert identity (no CA / no name check) but verifies the handshake
/// signature against the presented cert's key. The fingerprint pin (V10) is enforced
/// post-handshake by the application, not here.
#[derive(Debug)]
struct PinnedTrustVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedTrustVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for PinnedTrustVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn certs_and_key(node: &NodeCertificate) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let cert = CertificateDer::from(node.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(node.key_der.clone()));
    (vec![cert], key)
}

/// Build the QUIC **server** config: present `node`'s cert, require + accept any
/// client cert (the pin is enforced post-handshake), TLS 1.3 only, 0-RTT off.
pub fn quic_server_config(node: &NodeCertificate) -> anyhow::Result<quinn::ServerConfig> {
    let prov = provider();
    let (certs, key) = certs_and_key(node);
    let verifier = Arc::new(PinnedTrustVerifier {
        provider: prov.clone(),
    });
    let mut rustls_cfg = rustls::ServerConfig::builder_with_provider(prov)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    // V18 / ADR-2: control RPCs must not be replayable — no 0-RTT early data.
    rustls_cfg.max_early_data_size = 0;
    let server_cfg = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)?,
    ));
    Ok(server_cfg)
}

/// Build the QUIC **client** config: present `node`'s cert (mutual auth), accept any
/// server cert (the pin is enforced post-handshake), TLS 1.3 only, 0-RTT off.
pub fn quic_client_config(node: &NodeCertificate) -> anyhow::Result<quinn::ClientConfig> {
    let prov = provider();
    let (certs, key) = certs_and_key(node);
    let verifier = Arc::new(PinnedTrustVerifier {
        provider: prov.clone(),
    });
    let rustls_cfg = rustls::ClientConfig::builder_with_provider(prov)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    let client_cfg = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)?,
    ));
    Ok(client_cfg)
}

/// The SHA-256 fingerprint of the peer's leaf certificate (the V10 pin), read from a
/// completed handshake. The TLS layer verified the peer owns this cert's key.
pub(crate) fn peer_fingerprint(conn: &quinn::Connection) -> anyhow::Result<CertFingerprint> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| anyhow::anyhow!("peer presented no certificate"))?;
    let certs = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow::anyhow!("unexpected peer identity type"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty peer certificate chain"))?;
    Ok(crate::cert::CertFingerprint::of_der(leaf.as_ref()))
}
