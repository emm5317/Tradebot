//! Binance Spot WebSocket feed — multi-asset support.
//!
//! Subscribes to combined trade streams for all enabled assets.
//! Per-asset OHLC bars and volatility computation.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

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

/// Maximum number of 1-minute bars to retain (60 minutes of history).
const MAX_BARS: usize = 60;

/// EWMA decay factor (0.94 = RiskMetrics standard, adapted for 1-min intervals).
const EWMA_LAMBDA: f64 = 0.94;

/// Minutes per year for annualization: 365.25 * 24 * 60.
const MINUTES_PER_YEAR: f64 = 525_600.0;

/// Minimum number of bars needed to initialize EWMA from simple variance.
const EWMA_INIT_BARS: usize = 10;

/// Number of bars (plus 1 for returns) needed for realized vol.
const REALIZED_VOL_BARS: usize = 31;

/// A single 1-minute OHLC bar.
#[derive(Debug, Clone)]
struct OhlcBar {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// Per-asset Binance spot feed state.
#[derive(Debug)]
struct BinanceSpotAssetState {
    spot_price: f64,
    current_bar_minute: i64,
    current_open: f64,
    current_high: f64,
    current_low: f64,
    current_close: f64,
    current_volume: f64,
    bars_1m: VecDeque<OhlcBar>,
    realized_vol_30m: Option<f64>,
    ewma_vol_30m: Option<f64>,
    ewma_variance: f64,
}

impl Default for BinanceSpotAssetState {
    fn default() -> Self {
        Self {
            spot_price: 0.0,
            current_bar_minute: -1,
            current_open: 0.0,
            current_high: 0.0,
            current_low: f64::INFINITY,
            current_close: 0.0,
            current_volume: 0.0,
            bars_1m: VecDeque::with_capacity(MAX_BARS),
            realized_vol_30m: None,
            ewma_vol_30m: None,
            ewma_variance: 0.0,
        }
    }
}

impl BinanceSpotAssetState {
    fn volume_5m(&self) -> f64 {
        let closed_vol: f64 = self.bars_1m.iter().rev().take(5).map(|b| b.volume).sum();
        closed_vol + self.current_volume
    }

    fn handle_trade(&mut self, price: f64, qty: f64, trade_time_ms: i64) {
        self.spot_price = price;
        let current_minute = trade_time_ms / 60_000;

        if current_minute != self.current_bar_minute {
            if self.current_bar_minute >= 0 {
                let bar = OhlcBar {
                    open: self.current_open,
                    high: self.current_high,
                    low: self.current_low,
                    close: self.current_close,
                    volume: self.current_volume,
                };
                if self.bars_1m.len() >= MAX_BARS {
                    self.bars_1m.pop_front();
                }
                self.bars_1m.push_back(bar.clone());
                self.recompute_realized_vol();
                self.recompute_ewma_vol(&bar);
            }

            self.current_bar_minute = current_minute;
            self.current_open = price;
            self.current_high = price;
            self.current_low = price;
            self.current_close = price;
            self.current_volume = qty;
        } else {
            if price > self.current_high {
                self.current_high = price;
            }
            if price < self.current_low {
                self.current_low = price;
            }
            self.current_close = price;
            self.current_volume += qty;
        }
    }

    fn recompute_realized_vol(&mut self) {
        if self.bars_1m.len() < REALIZED_VOL_BARS {
            self.realized_vol_30m = None;
            return;
        }

        let start = self.bars_1m.len() - REALIZED_VOL_BARS;
        let closes: Vec<f64> = self.bars_1m.iter().skip(start).map(|b| b.close).collect();

        let log_returns: Vec<f64> = closes
            .windows(2)
            .filter(|w| w[0] > 0.0)
            .map(|w| (w[1] / w[0]).ln())
            .collect();

        if log_returns.len() < 2 {
            self.realized_vol_30m = None;
            return;
        }

        let n = log_returns.len() as f64;
        let mean = log_returns.iter().sum::<f64>() / n;
        let variance = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let sigma_1min = variance.sqrt();

        self.realized_vol_30m = Some(sigma_1min * MINUTES_PER_YEAR.sqrt());
    }

    fn recompute_ewma_vol(&mut self, bar: &OhlcBar) {
        if self.bars_1m.len() < 2 {
            return;
        }

        let prev_close = self.bars_1m[self.bars_1m.len() - 2].close;
        if prev_close <= 0.0 {
            return;
        }

        let log_return = (bar.close / prev_close).ln();

        if self.ewma_variance == 0.0 && self.bars_1m.len() >= EWMA_INIT_BARS {
            let start = self.bars_1m.len().saturating_sub(EWMA_INIT_BARS + 1);
            let closes: Vec<f64> = self.bars_1m.iter().skip(start).map(|b| b.close).collect();
            let returns: Vec<f64> = closes
                .windows(2)
                .filter(|w| w[0] > 0.0)
                .map(|w| (w[1] / w[0]).ln())
                .collect();
            if !returns.is_empty() {
                self.ewma_variance =
                    returns.iter().map(|r| r * r).sum::<f64>() / returns.len() as f64;
            }
        }

        self.ewma_variance =
            EWMA_LAMBDA * self.ewma_variance + (1.0 - EWMA_LAMBDA) * log_return * log_return;

        let sigma_1min = self.ewma_variance.sqrt();
        self.ewma_vol_30m = Some(sigma_1min * MINUTES_PER_YEAR.sqrt());
    }
}

/// Binance Spot WebSocket feed — multi-asset.
pub struct BinanceSpotFeed {
    base_ws_url: String,
    assets: Vec<CryptoAsset>,
    cancel: CancellationToken,
}

impl BinanceSpotFeed {
    pub fn new(base_ws_url: String, assets: Vec<CryptoAsset>, cancel: CancellationToken) -> Self {
        Self { base_ws_url, assets, cancel }
    }

    /// Build combined stream URL: e.g. wss://stream.binance.us:9443/stream?streams=btcusdt@trade/ethusdt@trade
    fn build_stream_url(&self) -> String {
        let streams: Vec<String> = self
            .assets
            .iter()
            .map(|a| format!("{}@trade", a.binance_symbol()))
            .collect();

        // Strip any existing path/query from base URL
        let base = self.base_ws_url
            .split("/ws/")
            .next()
            .unwrap_or(&self.base_ws_url)
            .trim_end_matches('/');

        format!("{}/stream?streams={}", base, streams.join("/"))
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
                info!("binance spot feed cancelled");
                return;
            }

            match self
                .connect_and_stream(&redis, &registry, &feed_health)
                .await
            {
                Ok(()) => {
                    warn!("binance spot ws closed by server, will reconnect");
                    backoff_secs = 1;
                }
                Err(e) => {
                    error!(error = %e, "binance spot ws disconnected");
                    let delay = Duration::from_secs(backoff_secs);
                    warn!(?delay, "reconnecting to binance spot ws");

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
        let url = self.build_stream_url();
        let request = url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!(url = %url, "binance spot ws connected");

        let (mut write, mut read) = ws_stream.split();

        // Build symbol → CryptoAsset lookup
        let symbol_map: HashMap<&str, CryptoAsset> = self
            .assets
            .iter()
            .map(|a| (a.binance_symbol_upper(), *a))
            .collect();

        let mut states: HashMap<CryptoAsset, BinanceSpotAssetState> = self
            .assets
            .iter()
            .map(|a| (*a, BinanceSpotAssetState::default()))
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
                            if let Some(asset) = parse_binance_spot_message_multi(&text, &mut states, &symbol_map) {
                                let feed_name = format!("binance_spot_{}", asset.short_name());
                                feed_health.record_update(&feed_name);
                                // Legacy BTC compat
                                if asset == CryptoAsset::BTC {
                                    feed_health.record_update("binance_spot");
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("binance spot ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    for (&asset, state) in &states {
                        if state.spot_price > 0.0 {
                            if let Some(cs) = registry.get(asset) {
                                cs.update_binance_spot(
                                    state.spot_price,
                                    state.realized_vol_30m,
                                    state.ewma_vol_30m,
                                    state.bars_1m.len(),
                                    state.volume_5m(),
                                );
                            }
                            flush_binance_spot_asset_state(asset, state, redis).await;
                        }
                    }
                }
            }
        }
    }
}

/// Parse a combined-stream Binance message, routing to the correct asset state.
fn parse_binance_spot_message_multi(
    text: &str,
    states: &mut HashMap<CryptoAsset, BinanceSpotAssetState>,
    symbol_map: &HashMap<&str, CryptoAsset>,
) -> Option<CryptoAsset> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;

    // Combined stream wraps in {"stream":"...","data":{...}}
    let data = msg.get("data").unwrap_or(&msg);
    let event_type = data.get("e").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "trade" {
        return None;
    }

    let symbol = data.get("s").and_then(|v| v.as_str())?;
    let asset = symbol_map.get(symbol).copied()?;

    let price = data
        .get("p")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())?;
    let qty = data
        .get("q")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())?;
    let trade_time_ms = data.get("T").and_then(|v| v.as_i64())?;

    if price > 0.0 {
        if let Some(state) = states.get_mut(&asset) {
            state.handle_trade(price, qty, trade_time_ms);
        }
    }

    Some(asset)
}

async fn flush_binance_spot_asset_state(asset: CryptoAsset, state: &BinanceSpotAssetState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "spot_price": state.spot_price,
        "realized_vol_30m": state.realized_vol_30m,
        "ewma_vol_30m": state.ewma_vol_30m,
        "bars_count": state.bars_1m.len(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let key = format!("crypto:binance_spot:{}", asset.short_name());
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
        warn!(error = %e, asset = %asset, "failed to write binance spot state to redis");
    }

    // BTC backward compat
    if asset == CryptoAsset::BTC {
        let _: Result<(), _> = redis
            .set(
                "crypto:binance_spot",
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

    fn make_symbol_map() -> HashMap<&'static str, CryptoAsset> {
        let mut m = HashMap::new();
        m.insert("BTCUSDT", CryptoAsset::BTC);
        m.insert("ETHUSDT", CryptoAsset::ETH);
        m
    }

    fn make_states() -> HashMap<CryptoAsset, BinanceSpotAssetState> {
        let mut m = HashMap::new();
        m.insert(CryptoAsset::BTC, BinanceSpotAssetState::default());
        m.insert(CryptoAsset::ETH, BinanceSpotAssetState::default());
        m
    }

    fn make_trade_msg(symbol: &str, price: f64, qty: f64, ts_ms: i64) -> String {
        serde_json::json!({
            "data": {
                "e": "trade",
                "s": symbol,
                "p": format!("{:.2}", price),
                "q": format!("{:.4}", qty),
                "T": ts_ms,
            }
        })
        .to_string()
    }

    #[test]
    fn test_parse_trade_btc() {
        let mut states = make_states();
        let symbol_map = make_symbol_map();
        let msg = make_trade_msg("BTCUSDT", 95000.50, 0.1, 1700000000000);
        let asset = parse_binance_spot_message_multi(&msg, &mut states, &symbol_map);
        assert_eq!(asset, Some(CryptoAsset::BTC));
        assert!((states[&CryptoAsset::BTC].spot_price - 95000.50).abs() < 0.01);
        assert_eq!(states[&CryptoAsset::ETH].spot_price, 0.0);
    }

    #[test]
    fn test_parse_trade_eth() {
        let mut states = make_states();
        let symbol_map = make_symbol_map();
        let msg = make_trade_msg("ETHUSDT", 3500.0, 1.0, 1700000000000);
        let asset = parse_binance_spot_message_multi(&msg, &mut states, &symbol_map);
        assert_eq!(asset, Some(CryptoAsset::ETH));
        assert!((states[&CryptoAsset::ETH].spot_price - 3500.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_unknown_symbol() {
        let mut states = make_states();
        let symbol_map = make_symbol_map();
        let msg = make_trade_msg("XYZUSDT", 100.0, 1.0, 1700000000000);
        let asset = parse_binance_spot_message_multi(&msg, &mut states, &symbol_map);
        assert_eq!(asset, None);
    }

    #[test]
    fn test_combined_stream_url() {
        let feed = BinanceSpotFeed::new(
            "wss://stream.binance.us:9443/ws/btcusd@trade".to_string(),
            vec![CryptoAsset::BTC, CryptoAsset::ETH, CryptoAsset::SOL],
            CancellationToken::new(),
        );
        let url = feed.build_stream_url();
        assert!(url.contains("btcusdt@trade"));
        assert!(url.contains("ethusdt@trade"));
        assert!(url.contains("solusdt@trade"));
        assert!(url.contains("/stream?streams="));
    }

    #[test]
    fn test_per_asset_ohlc_isolation() {
        let mut states = make_states();
        let symbol_map = make_symbol_map();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate BTC trades
        for i in 0..5 {
            let msg = make_trade_msg("BTCUSDT", 95000.0 + i as f64, 1.0, base_ms + i * minute_ms);
            parse_binance_spot_message_multi(&msg, &mut states, &symbol_map);
        }

        // ETH should have no bars
        assert_eq!(states[&CryptoAsset::ETH].bars_1m.len(), 0);
        assert!(states[&CryptoAsset::BTC].bars_1m.len() > 0 || states[&CryptoAsset::BTC].current_bar_minute >= 0);
    }
}
