//! Integrations-Test (#431): WebTransport-Roundtrip gegen den echten Endpoint.
//! AC-2: ein Test-Client empfaengt + dekodiert einen topic+msgpack+zstd-Frame.
//! AC-3: bei konfiguriertem Dashboard-Key wird eine Session ohne gueltiges Cookie abgewiesen.

use std::time::Duration;

use sentinel_dashboard_backend::{codec, tls, wt, AppState, Config};
use wtransport::{ClientConfig, Endpoint};

/// Startet den WT-Server auf einem ephemeren Port mit frischem self-signed Cert.
/// Gibt (Port, Cert-Hash-base64) zurueck.
async fn spawn_server(dashboard_api_key: Option<String>) -> (u16, String) {
    // Freien UDP-Port ermitteln.
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = sock.local_addr().unwrap().port();
    drop(sock);

    let dir = std::env::temp_dir().join(format!("wt-test-{}", uuid::Uuid::new_v4()));
    let cert = tls::generate(&dir, &["localhost", "127.0.0.1"]).unwrap();
    let hash = cert.cert_hash_b64.clone();

    let mut config = Config::from_env();
    config.dashboard_api_key = dashboard_api_key;
    config.wt_bind = format!("127.0.0.1:{port}");
    let mut state = AppState::new(config).unwrap();
    state.config = std::sync::Arc::new({
        let mut c = (*state.config).clone();
        c.cert_hash_b64 = Some(hash.clone());
        c
    });

    let (cp, kp) = (cert.cert_pem_path.clone(), cert.key_pem_path.clone());
    tokio::spawn(async move {
        let _ = wt::run_server(state, cp, kp).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await; // Server-Startup
    (port, hash)
}

fn client_endpoint(cert_hash_b64: &str) -> Endpoint<wtransport::endpoint::endpoint_side::Client> {
    use base64::Engine;
    let hash = base64::engine::general_purpose::STANDARD.decode(cert_hash_b64).unwrap();
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(
            hash.try_into().expect("sha256 = 32 bytes"),
        )])
        .build();
    Endpoint::client(config).unwrap()
}

#[tokio::test]
async fn ac2_client_receives_and_decodes_hello_frame() {
    let (port, hash) = spawn_server(None).await; // kein Key => WT offen
    let client = client_endpoint(&hash);
    let conn = client
        .connect(format!("https://127.0.0.1:{port}"))
        .await
        .expect("connect");

    // Server-initiierter uni-Stream traegt den hello-Frame.
    let mut recv = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni())
        .await
        .expect("uni stream in time")
        .expect("uni stream");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(Ok(Some(n))) = tokio::time::timeout(Duration::from_secs(5), recv.read(&mut chunk)).await {
        buf.extend_from_slice(&chunk[..n]);
    }
    let (topic, value): (String, serde_json::Value) = codec::decode_frame_as(&buf).expect("decode frame");
    assert_eq!(topic, "hello");
    assert_eq!(value["proto"], "topic-msgpack-zstd-v1");
}

#[tokio::test]
async fn ac3_session_without_cookie_rejected_when_key_set() {
    let (port, hash) = spawn_server(Some("topsecret".into())).await; // Key gesetzt => Auth Pflicht
    let client = client_endpoint(&hash);
    // Verbindung ohne Session-Cookie: Server akzeptiert die Session nicht (Drop) => kein uni-Frame.
    match client.connect(format!("https://127.0.0.1:{port}")).await {
        Ok(conn) => {
            let got = tokio::time::timeout(Duration::from_secs(2), conn.accept_uni()).await;
            assert!(
                got.is_err() || got.unwrap().is_err(),
                "ohne Cookie darf kein Frame/Stream kommen"
            );
        }
        Err(_) => { /* Verbindung schon beim Handshake abgewiesen — auch ok */ }
    }
}
