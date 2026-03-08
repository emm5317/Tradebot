//! Deribit DVOL (BTC volatility index) WebSocket feed.
//!
//! Optional feed, gated by config flag. Subscribes to public
//! `deribit_volatility_index.btc_usd` channel. No auth required.
//! Flushes DVOL to Redis key `crypto:deribit_dvol`.

use std::sync::Arc;
use std::time::Duration;

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::crypto_state::CryptoState;

/// Deribit DVOL state.
#[derive(Debug, Clone, Default)]
struct DeribitState {
    dvol: f64,
}

/// Deribit DVOL WebSocket feed.
pub struct DeribitFeed {
    ws_url: String,
    cancel: CancellationToken,
}

impl DeribitFeed {
    pub fn new(ws_url: String, cancel: CancellationToken) -> Self {
        Self { ws_url, cancel }
    }

    /// Run the feed with auto-reconnect. Writes to CryptoState + Redis.
    pub async fn run(&self, redis: RedisClient, crypto_state: Arc<CryptoState>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("deribit feed cancelled");
                return;
            }

            match self.connect_and_stream(&redis, &crypto_state).await {
                Ok(()) => {
                    info!("deribit ws closed cleanly");
                    return;
                }
                Err(e) => {
                    error!(error = %e, "deribit ws disconnected");
                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to deribit ws");

                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = self.cancel.cancelled() => return,
                    }
                    backoff_secs = (backoff_secs * 2).min(max_backoff);
                }
            }
        }
    }

    async fn connect_and_stream(
        &self,
        redis: &RedisClient,
        crypto_state: &CryptoState,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = self.ws_url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!("deribit ws connected");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to DVOL index (public, no auth)
        let subscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "public/subscribe",
            "params": {
                "channels": ["deribit_volatility_index.btc_usd"]
            }
        });
        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await?;

        let mut state = DeribitState::default();
        let mut flush_interval = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(());
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            parse_deribit_message(&text, &mut state);
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("deribit ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    if state.dvol > 0.0 {
                        crypto_state.update_deribit(state.dvol);
                        flush_deribit_state(&state, redis).await;
                    }
                }
            }
        }
    }
}

fn parse_deribit_message(text: &str, state: &mut DeribitState) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // Deribit uses JSON-RPC format
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if method != "subscription" {
        return;
    }

    let params = match msg.get("params") {
        Some(p) => p,
        None => return,
    };

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if channel == "deribit_volatility_index.btc_usd" {
        if let Some(data) = params.get("data") {
            if let Some(volatility) = data.get("volatility").and_then(|v| v.as_f64()) {
                state.dvol = volatility;
            }
        }
    }
}

async fn flush_deribit_state(state: &DeribitState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "dvol": state.dvol,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let result: Result<(), _> = redis
        .set(
            "crypto:deribit_dvol",
            summary.to_string().as_str(),
            Some(fred::types::Expiration::EX(60)),
            None,
            false,
        )
        .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to write deribit dvol to redis");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dvol_subscription() {
        let mut state = DeribitState::default();
        let msg = r#"{"method":"subscription","params":{"channel":"deribit_volatility_index.btc_usd","data":{"volatility":52.3,"estimated_delivery_price":95000.0}}}"#;
        parse_deribit_message(msg, &mut state);
        assert!((state.dvol - 52.3).abs() < 0.01);
    }

    #[test]
    fn test_ignore_non_subscription() {
        let mut state = DeribitState::default();
        let msg = r#"{"jsonrpc":"2.0","id":1,"result":["deribit_volatility_index.btc_usd"]}"#;
        parse_deribit_message(msg, &mut state);
        assert_eq!(state.dvol, 0.0);
    }
}
