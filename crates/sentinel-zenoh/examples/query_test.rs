//! Sendet eine Scoped Query an den laufenden Daemon und wartet auf Response.
//!
//! Nutzung: query_test [agent_id]
//! Default: agent_id=1
//!
//! Verbindet direkt zum Daemon-Peer (kein Multicast-Scouting noetig).

use sentinel_common::{AgentId, Tick};
use sentinel_zenoh::query::{QueryScope, ScopedQuery};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let agent_id: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    // Direkte Peer-Verbindung zum Daemon (Port aus journalctl ablesen)
    let connect_endpoint = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "tcp/127.0.0.1:38625".to_string());
    println!("Connecting to Zenoh peer at {connect_endpoint}...");

    let mut config = zenoh::Config::default();
    config
        .insert_json5("connect/endpoints", &format!("[\"{connect_endpoint}\"]"))
        .map_err(|e| anyhow::anyhow!("Config error: {e}"))?;

    let session = zenoh::open(config)
        .await
        .map_err(|e| anyhow::anyhow!("Zenoh open failed: {e}"))?;
    println!("Session opened, waiting for connection (1s)...");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Direkt Zenoh API nutzen statt SentinelBus (weniger Overhead fuer Test)
    let query = ScopedQuery::new(
        AgentId(99),
        QueryScope::Agent(AgentId(agent_id)),
        vec![],
        Tick(0),
        2000, // 2s deadline
        0,
    );

    let request_topic = format!("sentinel/query/agent/AGENT-{agent_id:02}/request");
    let response_topic = "sentinel/query/response/AGENT-99";

    println!("Subscribing to {response_topic}...");
    let sub = session
        .declare_subscriber(response_topic)
        .await
        .map_err(|e| anyhow::anyhow!("Subscribe failed: {e}"))?;

    let payload = serde_json::to_vec(&query)?;
    println!(
        "Publishing query to {request_topic} (query_id={})",
        query.query_id
    );
    session
        .put(&request_topic, payload)
        .await
        .map_err(|e| anyhow::anyhow!("Publish failed: {e}"))?;

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(2000);

    match tokio::time::timeout(deadline, sub.recv_async()).await {
        Ok(Ok(sample)) => {
            let elapsed = start.elapsed();
            let bytes = sample.payload().to_bytes();
            println!("Response received in {elapsed:?} ({} bytes)", bytes.len());
            if let Ok(resp) =
                serde_json::from_slice::<sentinel_zenoh::query::QueryResponse>(&bytes)
            {
                println!("  query_id:      {}", resp.query_id);
                println!("  response_tick: {}", resp.response_tick);
                println!("  payload_len:   {} bytes", resp.payload.len());
            }
            println!("AC-5 PASS: Response within deadline");
        }
        Ok(Err(_)) => {
            println!("Subscriber closed");
        }
        Err(_) => {
            println!("Timeout after {deadline:?}");
            println!("AC-5 FAIL: Query timed out");
        }
    }

    session.close().await.ok();
    Ok(())
}
