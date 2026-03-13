//! Coinbase Advanced Trade WebSocket feed — multi-asset support.
//!
//! Subscribes to `level2` and `market_trades` channels for all enabled
//! crypto assets. Routes updates to per-asset CryptoState via the registry.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::crypto_asset::CryptoAsset;
use crate::crypto_state_registry::CryptoStateRegistry;
use crate::feed_health::FeedHealth;

/// 5-minute rolling window for trade volume accumulation.
const VOLUME_WINDOW: Duration = Duration::from_secs(300);

/// Per-asset Coinbase feed state.
#[derive(Debug, Clone)]
struct CoinbaseAssetState {
    spot: f64,
    best_bid: f64,
    best_ask: f64,
    trade_volumes: VecDeque<(Instant, f64)>,
    trade_volume_5m: f64,
}

impl Default for CoinbaseAssetState {
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

impl CoinbaseAssetState {
    fn recompute_volume_5m(&mut self) {
        let cutoff = Instant::now() - VOLUME_WINDOW;
        while self
            .trade_volumes
            .front()
            .is_some_and(|(t, _)| *t < cutoff)
        {
            self.trade_volumes.pop_front();
        }
        self.trade_volume_5m = self.trade_volumes.iter().map(|(_, v)| v).sum();
    }
}

/// Coinbase WebSocket feed — multi-product.
pub struct CoinbaseFeed {
    ws_url: String,
    assets: Vec<CryptoAsset>,
    cancel: CancellationToken,
}

impl CoinbaseFeed {
    pub fn new(ws_url: String, assets: Vec<CryptoAsset>, cancel: CancellationToken) -> Self {
        Self { ws_url, assets, cancel }
    }

    /// Run the feed with auto-reconnect. Writes to per-asset CryptoState + Redis.
    pub async fn run(
        &self,
        redis: RedisClient,
        registry: Arc<CryptoStateRegistry>,
        feed_health: Arc<FeedHealth>,
    ) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("coinbase feed cancelled");
                return;
            }

            match self
                .connect_and_stream(&redis, &registry, &feed_health)
                .await
            {
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
        registry: &CryptoStateRegistry,
        feed_health: &FeedHealth,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = self.ws_url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!("coinbase ws connected");

        let (mut write, mut read) = ws_stream.split();

        // Build product IDs from enabled assets
        let product_ids: Vec<&str> = self.assets.iter().map(|a| a.coinbase_product_id()).collect();

        // Build product_id → CryptoAsset lookup
        let product_map: HashMap<&str, CryptoAsset> = self
            .assets
            .iter()
            .map(|a| (a.coinbase_product_id(), *a))
            .collect();

        let subscribe_ticker = serde_json::json!({
            "type": "subscribe",
            "product_ids": product_ids,
            "channel": "ticker"
        });
        write
            .send(Message::Text(subscribe_ticker.to_string().into()))
            .await?;

        let subscribe_trades = serde_json::json!({
            "type": "subscribe",
            "product_ids": product_ids,
            "channel": "market_trades"
        });
        write
            .send(Message::Text(subscribe_trades.to_string().into()))
            .await?;

        info!(products = ?product_ids, "coinbase subscribed to products");

        let mut states: HashMap<CryptoAsset, CoinbaseAssetState> = self
            .assets
            .iter()
            .map(|a| (*a, CoinbaseAssetState::default()))
            .collect();
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
                            if let Some(asset) = parse_coinbase_message_multi(&text, &mut states, &product_map) {
                                let feed_name = format!("coinbase_{}", asset.short_name());
                                feed_health.record_update(&feed_name);
                                // Also record the legacy "coinbase" name for BTC backward compat
                                if asset == CryptoAsset::BTC {
                                    feed_health.record_update("coinbase");
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("coinbase ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    for (&asset, state) in &mut states {
                        if state.spot > 0.0 {
                            state.recompute_volume_5m();
                            if let Some(cs) = registry.get(asset) {
                                cs.update_coinbase(
                                    state.spot,
                                    state.best_bid,
                                    state.best_ask,
                                    state.trade_volume_5m,
                                );
                            }
                            flush_coinbase_state(asset, state, redis).await;
                        }
                    }
                }
            }
        }
    }
}

/// Parse a Coinbase message, routing to the correct per-asset state.
/// Returns the asset that was updated, if any.
fn parse_coinbase_message_multi(
    text: &str,
    states: &mut HashMap<CryptoAsset, CoinbaseAssetState>,
    product_map: &HashMap<&str, CryptoAsset>,
) -> Option<CryptoAsset> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    let channel = msg.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    match channel {
        "ticker" | "ticker_batch" => {
            if let Some(events) = msg.get("events").and_then(|v| v.as_array()) {
                for event in events {
                    if let Some(tickers) = event.get("tickers").and_then(|v| v.as_array()) {
                        for ticker in tickers {
                            let product_id = ticker.get("product_id").and_then(|v| v.as_str());
                            let asset = product_id.and_then(|pid| product_map.get(pid)).copied();
                            if let Some(asset) = asset {
                                if let Some(state) = states.get_mut(&asset) {
                                    if let Some(price) = ticker
                                        .get("price")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f64>().ok())
                                    {
                                        if price > 0.0 {
                                            state.spot = price;
                                        }
                                    }
                                    if let Some(bid) = ticker
                                        .get("best_bid")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f64>().ok())
                                    {
                                        if bid > 0.0 {
                                            state.best_bid = bid;
                                        }
                                    }
                                    if let Some(ask) = ticker
                                        .get("best_ask")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f64>().ok())
                                    {
                                        if ask > 0.0 {
                                            state.best_ask = ask;
                                        }
                                    }
                                }
                                return Some(asset);
                            }
                        }
                    }
                }
            }
        }
        "market_trades" => {
            if let Some(events) = msg.get("events").and_then(|v| v.as_array()) {
                let mut updated_asset = None;
                for event in events {
                    if let Some(trades) = event.get("trades").and_then(|v| v.as_array()) {
                        for trade in trades {
                            let product_id = trade.get("product_id").and_then(|v| v.as_str());
                            let asset = product_id.and_then(|pid| product_map.get(pid)).copied();
                            if let Some(asset) = asset {
                                if let Some(state) = states.get_mut(&asset) {
                                    let size: f64 = trade
                                        .get("size")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0.0);
                                    if size > 0.0 {
                                        state.trade_volumes.push_back((Instant::now(), size));
                                    }
                                }
                                updated_asset = Some(asset);
                            }
                        }
                    }
                }
                return updated_asset;
            }
        }
        _ => {}
    }
    None
}

async fn flush_coinbase_state(asset: CryptoAsset, state: &CoinbaseAssetState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "spot": state.spot,
        "best_bid": state.best_bid,
        "best_ask": state.best_ask,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    // Per-asset key
    let key = format!("crypto:coinbase:{}", asset.short_name());
    let result: Result<(), _> = redis
        .set(
            key.as_str(),
            summary.to_string().as_str(),
            Some(fred::types::Expiration::EX(30)),
            None,
            false,
        )
        .await;

    if let Err(e) = result {
        warn!(error = %e, asset = %asset, "failed to write coinbase state to redis");
    }

    // BTC backward compat: also write to legacy key
    if asset == CryptoAsset::BTC {
        let _: Result<(), _> = redis
            .set(
                "crypto:coinbase",
                summary.to_string().as_str(),
                Some(fred::types::Expiration::EX(30)),
                None,
                false,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_product_map() -> HashMap<&'static str, CryptoAsset> {
        let mut m = HashMap::new();
        m.insert("BTC-USD", CryptoAsset::BTC);
        m.insert("ETH-USD", CryptoAsset::ETH);
        m
    }

    fn make_states() -> HashMap<CryptoAsset, CoinbaseAssetState> {
        let mut m = HashMap::new();
        m.insert(CryptoAsset::BTC, CoinbaseAssetState::default());
        m.insert(CryptoAsset::ETH, CoinbaseAssetState::default());
        m
    }

    #[test]
    fn test_parse_ticker_btc() {
        let mut states = make_states();
        let product_map = make_product_map();
        let msg = r#"{"channel":"ticker","events":[{"type":"ticker","tickers":[{"product_id":"BTC-USD","price":"95000.50","best_bid":"95000.00","best_ask":"95001.00"}]}]}"#;
        let asset = parse_coinbase_message_multi(msg, &mut states, &product_map);
        assert_eq!(asset, Some(CryptoAsset::BTC));
        assert!((states[&CryptoAsset::BTC].spot - 95000.50).abs() < 0.01);
        assert!((states[&CryptoAsset::BTC].best_bid - 95000.0).abs() < 0.01);
        assert!((states[&CryptoAsset::BTC].best_ask - 95001.0).abs() < 0.01);
        // ETH should be unchanged
        assert_eq!(states[&CryptoAsset::ETH].best_bid, 0.0);
    }

    #[test]
    fn test_parse_ticker_eth() {
        let mut states = make_states();
        let product_map = make_product_map();
        let msg = r#"{"channel":"ticker","events":[{"type":"ticker","tickers":[{"product_id":"ETH-USD","price":"3500.00","best_bid":"3499.50","best_ask":"3500.50"}]}]}"#;
        let asset = parse_coinbase_message_multi(msg, &mut states, &product_map);
        assert_eq!(asset, Some(CryptoAsset::ETH));
        assert!((states[&CryptoAsset::ETH].spot - 3500.0).abs() < 0.01);
        assert!((states[&CryptoAsset::ETH].best_bid - 3499.5).abs() < 0.01);
        assert!((states[&CryptoAsset::ETH].best_ask - 3500.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_ticker_batch() {
        let mut states = make_states();
        let product_map = make_product_map();
        let msg = r#"{"channel":"ticker_batch","events":[{"type":"ticker","tickers":[{"product_id":"BTC-USD","price":"95050.00","best_bid":"95000.00","best_ask":"95100.00"}]}]}"#;
        let asset = parse_coinbase_message_multi(msg, &mut states, &product_map);
        assert_eq!(asset, Some(CryptoAsset::BTC));
        assert!((states[&CryptoAsset::BTC].spot - 95050.0).abs() < 0.01);
    }

    #[test]
    fn test_multi_product_subscription_message() {
        let assets = vec![CryptoAsset::BTC, CryptoAsset::ETH, CryptoAsset::SOL];
        let product_ids: Vec<&str> = assets.iter().map(|a| a.coinbase_product_id()).collect();
        let sub = serde_json::json!({
            "type": "subscribe",
            "product_ids": product_ids,
            "channel": "ticker"
        });
        let sub_str = sub.to_string();
        assert!(sub_str.contains("BTC-USD"));
        assert!(sub_str.contains("ETH-USD"));
        assert!(sub_str.contains("SOL-USD"));
    }
}
