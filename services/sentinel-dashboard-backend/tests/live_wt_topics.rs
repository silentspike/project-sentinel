//! Ignored live smoke for #433 PR-A.
//!
//! Connects to the deployed dashboard backend and verifies that the WebTransport
//! connect snapshot contains the expected topic frames. Kept ignored so normal
//! test runs do not depend on the live VM.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use sentinel_console_plane::HelloManifest;
use sentinel_dashboard_backend::{auth, cas::EventLogCasResponse, codec};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use wtransport::{ClientConfig, Endpoint};

fn env_required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))
}

fn client_endpoint(
    cert_hash_b64: &str,
) -> anyhow::Result<Endpoint<wtransport::endpoint::endpoint_side::Client>> {
    let hash = base64::engine::general_purpose::STANDARD.decode(cert_hash_b64)?;
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(
            hash.try_into()
                .map_err(|_| anyhow::anyhow!("sha256 hash must be 32 bytes"))?,
        )])
        .build();
    Ok(Endpoint::client(config)?)
}

async fn read_one_frame(conn: &wtransport::Connection) -> anyhow::Result<(String, Value)> {
    let mut recv = tokio::time::timeout(Duration::from_secs(10), conn.accept_uni()).await??;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(Ok(Some(n))) =
        tokio::time::timeout(Duration::from_secs(10), recv.read(&mut chunk)).await
    {
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(codec::decode_frame_as(&buf)?)
}

async fn write_json_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(value)?;
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_json_frame<R, T>(reader: &mut R) -> anyhow::Result<T>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let mut payload = vec![0; u32::from_le_bytes(len) as usize];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

async fn fetch_ticket(origin: &str, session_cookie: &str) -> anyhow::Result<String> {
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = http
        .get(format!("{origin}/api/wt-ticket"))
        .header(
            reqwest::header::COOKIE,
            format!("{}={session_cookie}", auth::SESSION_COOKIE),
        )
        .send()
        .await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "ticket request failed with {}",
        resp.status()
    );
    let body = resp.json::<Value>().await?;
    body.get("ticket")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("ticket response did not contain a ticket"))
}

#[tokio::test]
#[ignore = "requires deployed VM and a live dashboard session cookie"]
async fn live_vm_connect_snapshot_contains_room_live_and_kpi() -> anyhow::Result<()> {
    let origin = std::env::var("SENTINEL_LIVE_HTTP_ORIGIN")
        .unwrap_or_else(|_| "https://10.0.0.240:8001".into());
    let wt_url =
        std::env::var("SENTINEL_LIVE_WT_URL").unwrap_or_else(|_| "https://10.0.0.240:8001".into());
    let cert_hash_b64 = env_required("SENTINEL_LIVE_CERT_HASH_B64")?;
    let session_cookie = env_required("SENTINEL_LIVE_SESSION_COOKIE")?;
    let ticket = fetch_ticket(&origin, &session_cookie).await?;

    let client = client_endpoint(&cert_hash_b64)?;
    let conn = client
        .connect(format!("{wt_url}?t={ticket}"))
        .await
        .map_err(|e| anyhow::anyhow!("connect live WT: {e}"))?;

    let mut frames = BTreeMap::new();
    for _ in 0..4 {
        let (topic, value) = read_one_frame(&conn).await?;
        frames.insert(topic, value);
    }

    println!(
        "live WT topics: {:?}",
        frames.keys().cloned().collect::<Vec<_>>()
    );
    for topic in ["hello", "agent_live", "room_live", "kpi"] {
        anyhow::ensure!(frames.contains_key(topic), "missing topic {topic}");
    }
    let agent_count = frames["agent_live"]["agents"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let room_count = frames["room_live"]["rooms"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    println!("live WT counts: agents={agent_count} rooms={room_count}");
    anyhow::ensure!(agent_count > 0, "agent_live snapshot must contain agents");
    anyhow::ensure!(room_count > 0, "room_live snapshot must contain rooms");
    anyhow::ensure!(
        frames["kpi"].get("kpi").is_some(),
        "kpi snapshot must contain kpi key"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires deployed VM and a live dashboard session cookie"]
async fn live_vm_event_log_cas_bi_stream_reassembles_events() -> anyhow::Result<()> {
    let origin = std::env::var("SENTINEL_LIVE_HTTP_ORIGIN")
        .unwrap_or_else(|_| "https://10.0.0.240:8001".into());
    let wt_url =
        std::env::var("SENTINEL_LIVE_WT_URL").unwrap_or_else(|_| "https://10.0.0.240:8001".into());
    let cert_hash_b64 = env_required("SENTINEL_LIVE_CERT_HASH_B64")?;
    let session_cookie = env_required("SENTINEL_LIVE_SESSION_COOKIE")?;
    let ticket = fetch_ticket(&origin, &session_cookie).await?;

    let client = client_endpoint(&cert_hash_b64)?;
    let conn = client
        .connect(format!("{wt_url}?t={ticket}"))
        .await
        .map_err(|e| anyhow::anyhow!("connect live WT: {e}"))?;

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("open live CAS bi stream: {e}"))?
        .await
        .map_err(|e| anyhow::anyhow!("live CAS bi stream ready: {e}"))?;
    write_json_frame(&mut send, &HelloManifest { have: vec![] }).await?;
    send.finish().await?;
    let response: EventLogCasResponse = read_json_frame(&mut recv).await?;

    println!(
        "live event_log_cas stats: events={} max_id={} blocks={}/{} bytes={} full={} dedup={:.4} savings={:.4}",
        response.stats.event_count,
        response.stats.max_event_id,
        response.delta.blocks.len(),
        response.manifest.len(),
        response.stats.delta_transfer_bytes,
        response.stats.full_state_bytes,
        response.stats.dedup_ratio,
        response.stats.savings_ratio,
    );
    anyhow::ensure!(response.topic == "event_log_cas", "unexpected CAS topic");
    anyhow::ensure!(
        response.stats.event_count > 0,
        "live CAS response must contain events"
    );
    anyhow::ensure!(
        !response.manifest.is_empty(),
        "live CAS manifest must contain block hashes"
    );
    anyhow::ensure!(
        !response.delta.blocks.is_empty(),
        "empty client manifest must receive missing blocks"
    );

    let compressed_by_hash = response
        .delta
        .blocks
        .iter()
        .cloned()
        .collect::<std::collections::HashMap<_, _>>();
    let mut ndjson = Vec::new();
    for hash in &response.manifest {
        let compressed = compressed_by_hash
            .get(hash)
            .ok_or_else(|| anyhow::anyhow!("missing block for manifest hash"))?;
        ndjson.extend(zstd::decode_all(compressed.as_slice())?);
    }
    let ids = std::str::from_utf8(&ndjson)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)?
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("event line missing numeric id"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(
        !ids.is_empty(),
        "reassembled CAS event log must not be empty"
    );
    anyhow::ensure!(
        ids.windows(2).all(|pair| pair[0] <= pair[1]),
        "event ids must be in ascending append order"
    );
    Ok(())
}
