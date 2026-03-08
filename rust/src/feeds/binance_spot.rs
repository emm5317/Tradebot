//! Binance Spot WebSocket feed for BTC-USDT.
//!
//! Subscribes to `btcusdt@trade` stream for real-time spot price,
//! accumulates 1-minute OHLC bars, and computes realized + EWMA volatility.
//! Flushes to Redis every 500ms.
//!
//! Ported from `python/data/binance_ws.py` (Phase 0.1).

use std::collections::VecDeque;
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

/// Binance spot feed state.
#[derive(Debug)]
struct BinanceSpotState {
    spot_price: f64,

    // Current bar tracking
    current_bar_minute: i64,
    current_open: f64,
    current_high: f64,
    current_low: f64,
    current_close: f64,
    current_volume: f64,

    // Closed bars
    bars_1m: VecDeque<OhlcBar>,

    // Volatility
    realized_vol_30m: Option<f64>,
    ewma_vol_30m: Option<f64>,
    ewma_variance: f64,
}

impl Default for BinanceSpotState {
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

impl BinanceSpotState {
    /// Handle a trade message: update spot, accumulate OHLC bars, compute vol.
    fn handle_trade(&mut self, price: f64, qty: f64, trade_time_ms: i64) {
        self.spot_price = price;
        let current_minute = trade_time_ms / 60_000;

        if current_minute != self.current_bar_minute {
            // Close the previous bar (if we had one)
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

            // Start a new bar
            self.current_bar_minute = current_minute;
            self.current_open = price;
            self.current_high = price;
            self.current_low = price;
            self.current_close = price;
            self.current_volume = qty;
        } else {
            // Update current bar
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

    /// Compute 30-min annualized realized volatility from 1-min log returns.
    fn recompute_realized_vol(&mut self) {
        if self.bars_1m.len() < REALIZED_VOL_BARS {
            self.realized_vol_30m = None;
            return;
        }

        // Use last 31 closes for 30 returns
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

    /// Update EWMA volatility with the latest bar's return.
    ///
    /// Formula: variance_t = lambda * variance_{t-1} + (1 - lambda) * r_t^2
    fn recompute_ewma_vol(&mut self, bar: &OhlcBar) {
        if self.bars_1m.len() < 2 {
            return;
        }

        let prev_close = self.bars_1m[self.bars_1m.len() - 2].close;
        if prev_close <= 0.0 {
            return;
        }

        let log_return = (bar.close / prev_close).ln();

        // Initialize EWMA variance from simple variance if needed
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

/// Binance Spot WebSocket feed.
pub struct BinanceSpotFeed {
    ws_url: String,
    cancel: CancellationToken,
}

impl BinanceSpotFeed {
    pub fn new(ws_url: String, cancel: CancellationToken) -> Self {
        Self { ws_url, cancel }
    }

    /// Run the feed with auto-reconnect. Writes to CryptoState + Redis.
    pub async fn run(&self, redis: RedisClient, crypto_state: Arc<CryptoState>) {
        let mut backoff_secs = 1u64;
        let max_backoff = 30u64;

        loop {
            if self.cancel.is_cancelled() {
                info!("binance spot feed cancelled");
                return;
            }

            match self.connect_and_stream(&redis, &crypto_state).await {
                Ok(()) => {
                    info!("binance spot ws closed cleanly");
                    return;
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
        crypto_state: &CryptoState,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = self.ws_url.as_str().into_client_request()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        info!("binance spot ws connected");

        let (mut write, mut read) = ws_stream.split();
        let mut state = BinanceSpotState::default();
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
                            parse_binance_spot_message(&text, &mut state);
                        }
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Err("binance spot ws stream ended".into()),
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    if state.spot_price > 0.0 {
                        crypto_state.update_binance_spot(
                            state.spot_price,
                            state.realized_vol_30m,
                            state.ewma_vol_30m,
                            state.bars_1m.len(),
                        );
                        flush_binance_spot_state(&state, redis).await;
                    }
                }
            }
        }
    }
}

fn parse_binance_spot_message(text: &str, state: &mut BinanceSpotState) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // Binance trade stream format: {"e":"trade","p":"95000.50","q":"0.1","T":1700000000000}
    let event_type = msg.get("e").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "trade" {
        return;
    }

    let price = msg
        .get("p")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok());
    let qty = msg
        .get("q")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok());
    let trade_time_ms = msg.get("T").and_then(|v| v.as_i64());

    if let (Some(price), Some(qty), Some(ts)) = (price, qty, trade_time_ms) {
        if price > 0.0 {
            state.handle_trade(price, qty, ts);
        }
    }
}

async fn flush_binance_spot_state(state: &BinanceSpotState, redis: &RedisClient) {
    let summary = serde_json::json!({
        "spot_price": state.spot_price,
        "realized_vol_30m": state.realized_vol_30m,
        "ewma_vol_30m": state.ewma_vol_30m,
        "bars_count": state.bars_1m.len(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let result: Result<(), _> = redis
        .set(
            "crypto:binance_spot",
            summary.to_string().as_str(),
            Some(fred::types::Expiration::EX(30)),
            None,
            false,
        )
        .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to write binance spot state to redis");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade_msg(price: f64, qty: f64, ts_ms: i64) -> String {
        serde_json::json!({
            "e": "trade",
            "s": "BTCUSDT",
            "p": format!("{:.2}", price),
            "q": format!("{:.4}", qty),
            "T": ts_ms,
        })
        .to_string()
    }

    #[test]
    fn test_parse_trade() {
        let mut state = BinanceSpotState::default();
        let msg = make_trade_msg(95000.50, 0.1, 1700000000000);
        parse_binance_spot_message(&msg, &mut state);
        assert!((state.spot_price - 95000.50).abs() < 0.01);
    }

    #[test]
    fn test_ignores_non_trade_events() {
        let mut state = BinanceSpotState::default();
        let msg = r#"{"e":"kline","s":"BTCUSDT"}"#;
        parse_binance_spot_message(msg, &mut state);
        assert_eq!(state.spot_price, 0.0);
    }

    #[test]
    fn test_bar_rollover_on_minute_boundary() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Trades in minute 0
        state.handle_trade(100.0, 1.0, base_ms);
        state.handle_trade(105.0, 1.0, base_ms + 10_000);
        state.handle_trade(98.0, 1.0, base_ms + 20_000);
        assert_eq!(state.bars_1m.len(), 0, "bar not closed yet");

        // First trade in minute 1 closes minute 0's bar
        state.handle_trade(110.0, 1.0, base_ms + minute_ms);
        assert_eq!(state.bars_1m.len(), 1);

        let bar = &state.bars_1m[0];
        assert!((bar.open - 100.0).abs() < 0.01);
        assert!((bar.high - 105.0).abs() < 0.01);
        assert!((bar.low - 98.0).abs() < 0.01);
        assert!((bar.close - 98.0).abs() < 0.01);
    }

    #[test]
    fn test_realized_vol_needs_31_bars() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 30 bars (need 31 for vol)
        for i in 0..31 {
            let price = 100.0 + (i as f64) * 0.1;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        assert_eq!(state.bars_1m.len(), 30);
        assert!(state.realized_vol_30m.is_none());

        // 32nd trade closes 31st bar
        state.handle_trade(103.1, 1.0, base_ms + 31 * minute_ms);
        assert_eq!(state.bars_1m.len(), 31);
        assert!(state.realized_vol_30m.is_some());
    }

    #[test]
    fn test_ewma_vol_initialization() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 11 bars (10 needed for EWMA init)
        for i in 0..12 {
            let price = 100.0 + (i as f64) * 0.5;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        assert!(state.bars_1m.len() >= EWMA_INIT_BARS);
        assert!(state.ewma_vol_30m.is_some());
        assert!(state.ewma_variance > 0.0);
    }

    #[test]
    fn test_ewma_convergence() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 40 bars with constant price → vol should converge toward 0
        for i in 0..41 {
            state.handle_trade(100.0, 1.0, base_ms + (i as i64) * minute_ms);
        }

        if let Some(vol) = state.ewma_vol_30m {
            assert!(vol < 0.01, "EWMA vol should be near 0 for constant price, got {vol}");
        }
    }

    #[test]
    fn test_max_bars_cap() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 70 bars
        for i in 0..71 {
            let price = 100.0 + (i as f64) * 0.01;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }

        assert!(state.bars_1m.len() <= MAX_BARS);
    }

    #[test]
    fn test_vol_annualization() {
        // Verify annualization factor is applied
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate bars with ~1% per-minute moves (very high vol)
        for i in 0..32 {
            let price = 100.0 * (1.0 + 0.01 * (if i % 2 == 0 { 1.0 } else { -1.0 }));
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }

        if let Some(vol) = state.realized_vol_30m {
            // Annualized vol from ~1% 1-min moves should be very high
            assert!(vol > 1.0, "Annualized vol should be high for 1% per-minute moves, got {vol}");
        }
    }
}
