//! Coinbase Advanced Trade WebSocket feed for BTC-USD.
//!
//! Subscribes to the `level2` channel (public, no auth) to maintain
//! best bid/ask/mid. Flushes to Redis every 500ms for Python model consumption.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::crypto_state::CryptoState;
use crate::feed_health::FeedHealth;

/// 5-minute rolling window for trade volume accumulation.
const VOLUME_WINDOW: Duration = Duration::from_secs(300);

/// Coinbase feed state written to Redis.
#[derive(Debug, Clone)]
struct CoinbaseState {
    spot: f64,
    best_bid: f64,
    best_ask: f64,
    /// Trade-by-trade volume accumulation for 5-min rolling window.
    trade_volumes: VecDeque<(Instant, f64)>,
    /// Cached 5-minute rolling trade volume (BTC).
    trade_volume_5m: f64,
}

impl Default for CoinbaseState {
    fn default() -> Self {
        Self {
            spot: 0.0,
            best_bid: 0.0,
            best_ask: 0.0,
            trade_volumes: VecDeque::new(),
            trade_volume_5m: 0.0,
        }
    }
}

impl CoinbaseState {
    /// Recompute 5-minute rolling trade volume, evicting stale entries.
    fn recompute_volume_5m(&mut self) {
        let cutoff = Instant::now() - VOLUME_WINDOW;
        while self.trade_volumes.front().map_or(false, |(t, _)| *t < cutoff) {
            self.trade_volumes.pop_front();
        }
        self.trade_volume_5m = self.trade_volumes.iter().map(|(_, v)| v).sum();
    }
}

/// Coinbase WebSocket feed for BTC-USD spot price.
pub struct CoinbaseFeed {
    ws_url: String,
    cancel: CancellationToken,
}

impl CoinbaseFeed {
    pub fn new(ws_url: String, cancel: CancellationToken) -> Self {
        Self { ws_url, cancel }
    }

    /// Run the feed with auto-reconnect. Writes to CryptoState + Redis.
    pub async fn run(&self, redis: RedisClient, crypto_state: Arc<CryptoState>, feed_health: Arc<FeedHealth>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("coinbase feed cancelled");
                return;
            }

            match self.connect_and_stream(&redis, &crypto_state, &feed_health).await {
                Ok(()) => {
                    warn!("coinbase ws closed by server, will reconnect");
                    backoff_secs = 1;
                }
                Err(e) => {
                    error!(error = %e, "coinbase ws disconnected");
                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to coinbase ws");

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
        feed_health: &FeedHealth,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = self.ws_url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!("coinbase ws connected");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to level2 + market_trades channels for BTC-USD (public, no auth needed)
        let subscribe_l2 = serde_json::json!({
            "type": "subscribe",
            "product_ids": ["BTC-USD"],
            "channel": "level2"
        });
        write
            .send(Message::Text(subscribe_l2.to_string().into()))
            .await?;

        let subscribe_trades = serde_json::json!({
            "type": "subscribe",
            "product_ids": ["BTC-USD"],
            "channel": "market_trades"
        });
        write
            .send(Message::Text(subscribe_trades.to_string().into()))
            .await?;

        let mut state = CoinbaseState::default();
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
                            parse_coinbase_message(&text, &mut state);
                            feed_health.record_update("coinbase");
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("coinbase ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    if state.spot > 0.0 {
                        state.recompute_volume_5m();
                        crypto_state.update_coinbase(
                            state.spot,
                            state.best_bid,
                            state.best_ask,
                            state.trade_volume_5m,
                        );
                        flush_coinbase_state(&state, redis).await;
                    }
                }
            }
        }
    }
}

fn parse_coinbase_message(text: &str, state: &mut CoinbaseState) {
    // Parse the Coinbase level2 message
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    let channel = msg.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    match channel {
        "l2_data" => {
            // Level 2 updates contain bids/asks
            if let Some(events) = msg.get("events").and_then(|v| v.as_array()) {
                for event in events {
                    if let Some(updates) = event.get("updates").and_then(|v| v.as_array()) {
                        for update in updates {
                            let side = update
                                .get("side")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let price: f64 = update
                                .get("price_level")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0.0);

                            if price > 0.0 {
                                match side {
                                    "bid" => {
                                        if price > state.best_bid {
                                            state.best_bid = price;
                                        }
                                    }
                                    "offer" => {
                                        if state.best_ask == 0.0 || price < state.best_ask {
                                            state.best_ask = price;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                // Update spot as mid of bid/ask
                if state.best_bid > 0.0 && state.best_ask > 0.0 {
                    state.spot = (state.best_bid + state.best_ask) / 2.0;
                }
            }
        }
        "market_trades" => {
            // Extract trade sizes for volume tracking
            if let Some(events) = msg.get("events").and_then(|v| v.as_array()) {
                for event in events {
                    if let Some(trades) = event.get("trades").and_then(|v| v.as_array()) {
                        for trade in trades {
                            let size: f64 = trade
                                .get("size")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0.0);
                            if size > 0.0 {
                                state.trade_volumes.push_back((Instant::now(), size));
                            }
                        }
                    }
                }
            }
        }
        "ticker" | "ticker_batch" => {
            // Fallback: use ticker price if available
            if let Some(price) = msg
                .get("events")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|e| e.get("tickers"))
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|t| t.get("price"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
            {
                if price > 0.0 {
                    state.spot = price;
                }
            }
        }
        _ => {}
    }
}

async fn flush_coinbase_state(state: &CoinbaseState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "spot": state.spot,
        "best_bid": state.best_bid,
        "best_ask": state.best_ask,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let result: Result<(), _> = redis
        .set(
            "crypto:coinbase",
            summary.to_string().as_str(),
            Some(fred::types::Expiration::EX(30)),
            None,
            false,
        )
        .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to write coinbase state to redis");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_l2_bid() {
        let mut state = CoinbaseState::default();
        let msg = r#"{"channel":"l2_data","events":[{"type":"update","updates":[{"side":"bid","price_level":"95000.50","new_quantity":"1.5"}]}]}"#;
        parse_coinbase_message(msg, &mut state);
        assert!((state.best_bid - 95000.50).abs() < 0.01);
    }

    #[test]
    fn test_parse_l2_ask() {
        let mut state = CoinbaseState::default();
        let msg = r#"{"channel":"l2_data","events":[{"type":"update","updates":[{"side":"offer","price_level":"95100.00","new_quantity":"2.0"}]}]}"#;
        parse_coinbase_message(msg, &mut state);
        assert!((state.best_ask - 95100.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_l2_mid_price() {
        let mut state = CoinbaseState::default();
        let bid_msg = r#"{"channel":"l2_data","events":[{"type":"update","updates":[{"side":"bid","price_level":"95000.00","new_quantity":"1.0"}]}]}"#;
        let ask_msg = r#"{"channel":"l2_data","events":[{"type":"update","updates":[{"side":"offer","price_level":"95100.00","new_quantity":"1.0"}]}]}"#;
        parse_coinbase_message(bid_msg, &mut state);
        parse_coinbase_message(ask_msg, &mut state);
        assert!((state.spot - 95050.0).abs() < 0.01);
    }
}
