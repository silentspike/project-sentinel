//! NATS JetStream Consumer fuer Judge-Alerts.
//!
//! Subscribed auf `sentinel.judge.alert.>` und leitet Alerts
//! via mpsc Channel an den ECS Tick-Loop weiter.

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Alert vom Judge-Service (JSON auf NATS).
#[derive(Debug, Clone, Deserialize)]
pub struct JudgeAlert {
    /// Agent-ID im Format AGENT-XX.
    pub agent_id: String,
    /// Alert-Typ: drift, quality, fatigue, swap.
    #[serde(rename = "type")]
    pub alert_type: String,
    /// Schweregrad: none, mild, moderate, critical.
    pub severity: String,
    /// Numerischer Score (z.B. drift_score, fatigue_score).
    pub score: f64,
    /// Details zur Analyse.
    pub details: String,
}

/// Startet den NATS Consumer als async Task.
///
/// Subscribed auf den SENTINEL_JUDGE Stream und parsed eingehende
/// Judge-Alerts. Alerts werden via `alert_tx` an den Orchestrator gesendet.
pub async fn run(nats_url: &str, alert_tx: mpsc::Sender<JudgeAlert>) {
    let client = match async_nats::connect(nats_url).await {
        Ok(c) => {
            info!(url = nats_url, "NATS Connected");
            c
        }
        Err(e) => {
            error!(url = nats_url, error = %e, "NATS Verbindung fehlgeschlagen");
            return;
        }
    };

    let jetstream = async_nats::jetstream::new(client);

    // Stream holen
    let stream = match jetstream.get_stream("SENTINEL_JUDGE").await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "NATS Stream SENTINEL_JUDGE nicht gefunden");
            return;
        }
    };

    // Durable Pull Consumer auf dem Stream erstellen/holen
    let consumer = match stream
        .get_or_create_consumer(
            "sentinel-daemon",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("sentinel-daemon".to_string()),
                filter_subject: "sentinel.judge.alert.>".to_string(),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                max_deliver: 3,
                ..Default::default()
            },
        )
        .await
    {
        Ok(c) => {
            info!("Subscribed to sentinel.judge.alert.>");
            c
        }
        Err(e) => {
            error!(error = %e, "NATS Consumer Erstellung fehlgeschlagen");
            return;
        }
    };

    // Nachrichten verarbeiten via Pull-Consumer iteration
    use futures::StreamExt;

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            error!(error = %e, "NATS Messages-Stream fehlgeschlagen");
            return;
        }
    };

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "NATS Message-Fehler, continue");
                continue;
            }
        };

        match serde_json::from_slice::<JudgeAlert>(&msg.payload) {
            Ok(alert) => {
                info!(
                    agent_id = %alert.agent_id,
                    alert_type = %alert.alert_type,
                    severity = %alert.severity,
                    score = alert.score,
                    "Judge Alert empfangen"
                );

                if alert_tx.send(alert).await.is_err() {
                    warn!("Alert-Channel geschlossen, beende NATS Consumer");
                    break;
                }
            }
            Err(e) => {
                warn!(error = %e, "Judge Alert JSON Parse-Fehler");
            }
        }

        if let Err(e) = msg.ack().await {
            warn!(error = %e, "NATS Message ACK fehlgeschlagen");
        }
    }
}
