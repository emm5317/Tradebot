//! Binance Futures WebSocket feed for BTCUSDT perpetual.
//!
//! Subscribes to aggTrade, depth, and markPrice streams.
//! Maintains perp price, mark price, funding rate, and order book imbalance.
//! Flushes to Redis every 500ms.

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

/// Binance futures state written to Redis.
#[derive(Debug, Clone)]
struct BinanceFuturesState {
    perp_price: f64,
    mark_price: f64,
    funding_rate: f64,
    best_bid: f64,
    best_ask: f64,
    bid_qty: f64,
    ask_qty: f64,
}

impl Default for BinanceFuturesState {
    fn default() -> Self {
        Self {
            perp_price: 0.0,
            mark_price: 0.0,
            funding_rate: 0.0,
            best_bid: 0.0,
            best_ask: 0.0,
            bid_qty: 0.0,
            ask_qty: 0.0,
        }
    }
}

impl BinanceFuturesState {
    /// Order book imbalance: bid_qty / (bid_qty + ask_qty). >0.5 = buy pressure.
    fn obi(&self) -> f64 {
        let total = self.bid_qty + self.ask_qty;
        if total <= 0.0 {
            return 0.5;
        }
        self.bid_qty / total
    }

    /// Basis: (perp - spot equivalent via mark). Positive = contango.
    fn basis(&self) -> f64 {
        if self.mark_price <= 0.0 {
            return 0.0;
        }
        self.perp_price - self.mark_price
    }
}

/// Binance Futures WebSocket feed.
pub struct BinanceFuturesFeed {
    ws_url: String,
    cancel: CancellationToken,
}

impl BinanceFuturesFeed {
    pub fn new(ws_url: String, cancel: CancellationToken) -> Self {
        Self { ws_url, cancel }
    }

    /// Run the feed with auto-reconnect. Writes to CryptoState + Redis.
    pub async fn run(&self, redis: RedisClient, crypto_state: Arc<CryptoState>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("binance futures feed cancelled");
                return;
            }

            match self.connect_and_stream(&redis, &crypto_state).await {
                Ok(()) => {
                    info!("binance futures ws closed cleanly");
                    return;
                }
                Err(e) => {
                    error!(error = %e, "binance futures ws disconnected");
                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to binance futures ws");

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
        // Combined stream URL: aggTrade + depth@100ms + markPrice@1s
        let url = format!(
            "{}/stream?streams=btcusdt@aggTrade/btcusdt@depth@100ms/btcusdt@markPrice@1s",
            self.ws_url
        );
        let request = url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!("binance futures ws connected");

        let (mut write, mut read) = ws_stream.split();
        let mut state = BinanceFuturesState::default();
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
                            parse_binance_futures_message(&text, &mut state);
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("binance futures ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    if state.perp_price > 0.0 {
                        crypto_state.update_binance_futures(
                            state.perp_price,
                            state.mark_price,
                            state.funding_rate,
                            state.obi(),
                        );
                        flush_binance_futures_state(&state, redis).await;
                    }
                }
            }
        }
    }
}

fn parse_binance_futures_message(text: &str, state: &mut BinanceFuturesState) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // Combined stream wraps messages in {"stream": "...", "data": {...}}
    let data = msg.get("data").unwrap_or(&msg);
    let event_type = data.get("e").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "aggTrade" => {
            // Aggregate trade: last price
            if let Some(price) = data.get("p").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                state.perp_price = price;
            }
        }
        "depthUpdate" => {
            // Depth update: best bid/ask from top of book
            if let Some(bids) = data.get("b").and_then(|v| v.as_array()) {
                if let Some(top_bid) = bids.first().and_then(|v| v.as_array()) {
                    if top_bid.len() >= 2 {
                        if let (Some(price), Some(qty)) = (
                            top_bid[0].as_str().and_then(|s| s.parse::<f64>().ok()),
                            top_bid[1].as_str().and_then(|s| s.parse::<f64>().ok()),
                        ) {
                            state.best_bid = price;
                            state.bid_qty = qty;
                        }
                    }
                }
            }
            if let Some(asks) = data.get("a").and_then(|v| v.as_array()) {
                if let Some(top_ask) = asks.first().and_then(|v| v.as_array()) {
                    if top_ask.len() >= 2 {
                        if let (Some(price), Some(qty)) = (
                            top_ask[0].as_str().and_then(|s| s.parse::<f64>().ok()),
                            top_ask[1].as_str().and_then(|s| s.parse::<f64>().ok()),
                        ) {
                            state.best_ask = price;
                            state.ask_qty = qty;
                        }
                    }
                }
            }
        }
        "markPriceUpdate" => {
            // Mark price and funding rate
            if let Some(mark) = data.get("p").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                state.mark_price = mark;
            }
            if let Some(rate) = data.get("r").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                state.funding_rate = rate;
            }
        }
        _ => {}
    }
}

async fn flush_binance_futures_state(state: &BinanceFuturesState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "perp_price": state.perp_price,
        "mark_price": state.mark_price,
        "funding_rate": state.funding_rate,
        "basis": state.basis(),
        "obi": state.obi(),
        "best_bid": state.best_bid,
        "best_ask": state.best_ask,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let result: Result<(), _> = redis
        .set(
            "crypto:binance_futures",
            summary.to_string().as_str(),
            Some(fred::types::Expiration::EX(30)),
            None,
            false,
        )
        .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to write binance futures state to redis");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_agg_trade() {
        let mut state = BinanceFuturesState::default();
        let msg = r#"{"data":{"e":"aggTrade","s":"BTCUSDT","p":"95234.50","q":"0.1"}}"#;
        parse_binance_futures_message(msg, &mut state);
        assert!((state.perp_price - 95234.50).abs() < 0.01);
    }

    #[test]
    fn test_parse_mark_price() {
        let mut state = BinanceFuturesState::default();
        let msg = r#"{"data":{"e":"markPriceUpdate","s":"BTCUSDT","p":"95200.00","r":"0.00015"}}"#;
        parse_binance_futures_message(msg, &mut state);
        assert!((state.mark_price - 95200.0).abs() < 0.01);
        assert!((state.funding_rate - 0.00015).abs() < 0.00001);
    }

    #[test]
    fn test_parse_depth_update() {
        let mut state = BinanceFuturesState::default();
        let msg = r#"{"data":{"e":"depthUpdate","b":[["95000.00","1.5"]],"a":[["95100.00","2.0"]]}}"#;
        parse_binance_futures_message(msg, &mut state);
        assert!((state.best_bid - 95000.0).abs() < 0.01);
        assert!((state.best_ask - 95100.0).abs() < 0.01);
        assert!((state.bid_qty - 1.5).abs() < 0.01);
    }

    #[test]
    fn test_obi_balanced() {
        let state = BinanceFuturesState {
            bid_qty: 10.0,
            ask_qty: 10.0,
            ..Default::default()
        };
        assert!((state.obi() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_obi_buy_pressure() {
        let state = BinanceFuturesState {
            bid_qty: 30.0,
            ask_qty: 10.0,
            ..Default::default()
        };
        assert!((state.obi() - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_basis() {
        let state = BinanceFuturesState {
            perp_price: 95300.0,
            mark_price: 95200.0,
            ..Default::default()
        };
        assert!((state.basis() - 100.0).abs() < 0.01);
    }
}
