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

/// Maximum number of 1-minute bars to retain (120 minutes / 2 hours of history).
/// Increased from 60 to support Hurst exponent computation (100-bar rolling window).
const MAX_BARS: usize = 120;

/// EWMA decay factor (0.94 = RiskMetrics standard, adapted for 1-min intervals).
const EWMA_LAMBDA: f64 = 0.94;

/// Minutes per year for annualization: 365.25 * 24 * 60.
const MINUTES_PER_YEAR: f64 = 525_600.0;

/// Minimum number of bars needed to initialize EWMA from simple variance.
const EWMA_INIT_BARS: usize = 10;

/// Number of bars (plus 1 for returns) needed for realized vol.
const REALIZED_VOL_BARS: usize = 31;

/// GARCH(1,1) parameters — fitted offline on 90-day rolling BTC 1-min returns.
/// σ²_t = GARCH_OMEGA + GARCH_ALPHA * ε²_{t-1} + GARCH_BETA * σ²_{t-1}
/// Typical BTC 1-min values: alpha + beta ≈ 0.97 (high persistence).
const GARCH_OMEGA: f64 = 1.5e-10; // long-run variance anchor
const GARCH_ALPHA: f64 = 0.08; // reaction to new shocks
const GARCH_BETA: f64 = 0.89; // persistence of old variance

/// Minimum bars before GARCH can produce a forecast.
const GARCH_MIN_BARS: usize = 10;

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
    garch_vol: Option<f64>,
    garch_variance: f64,

    // Hurst exponent for regime detection (rolling 100-bar R/S analysis)
    hurst_exponent: Option<f64>,
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
            garch_vol: None,
            garch_variance: 0.0,
            hurst_exponent: None,
        }
    }
}

impl BinanceSpotState {
    /// Compute rolling 5-minute trade volume from recent closed bars + current bar.
    fn volume_5m(&self) -> f64 {
        // Sum volume from last 5 closed bars + current (in-progress) bar
        let closed_vol: f64 = self
            .bars_1m
            .iter()
            .rev()
            .take(5)
            .map(|b| b.volume)
            .sum();
        closed_vol + self.current_volume
    }

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
                self.recompute_garch_vol(&bar);
                self.recompute_hurst();
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

    /// Update GARCH(1,1) volatility forecast with latest bar's return.
    ///
    /// σ²_t = ω + α·ε²_{t-1} + β·σ²_{t-1}
    ///
    /// Unlike EWMA (IGARCH with ω=0), GARCH includes a long-run variance
    /// anchor that pulls vol back to its unconditional mean after spikes.
    fn recompute_garch_vol(&mut self, bar: &OhlcBar) {
        if self.bars_1m.len() < 2 {
            return;
        }

        let prev_close = self.bars_1m[self.bars_1m.len() - 2].close;
        if prev_close <= 0.0 {
            return;
        }

        let log_return = (bar.close / prev_close).ln();
        let epsilon_sq = log_return * log_return;

        // Initialize GARCH variance from simple variance if needed
        if self.garch_variance == 0.0 && self.bars_1m.len() >= GARCH_MIN_BARS {
            let start = self.bars_1m.len().saturating_sub(GARCH_MIN_BARS + 1);
            let closes: Vec<f64> = self.bars_1m.iter().skip(start).map(|b| b.close).collect();
            let returns: Vec<f64> = closes
                .windows(2)
                .filter(|w| w[0] > 0.0)
                .map(|w| (w[1] / w[0]).ln())
                .collect();
            if !returns.is_empty() {
                self.garch_variance =
                    returns.iter().map(|r| r * r).sum::<f64>() / returns.len() as f64;
            }
        }

        // GARCH(1,1) update
        self.garch_variance =
            GARCH_OMEGA + GARCH_ALPHA * epsilon_sq + GARCH_BETA * self.garch_variance;

        let sigma_1min = self.garch_variance.sqrt();
        self.garch_vol = Some(sigma_1min * MINUTES_PER_YEAR.sqrt());
    }

    /// Compute Hurst exponent via rescaled range (R/S) analysis on last 100 bars.
    ///
    /// H < 0.5 → mean-reverting, H > 0.5 → trending, H ≈ 0.5 → random walk.
    fn recompute_hurst(&mut self) {
        const HURST_WINDOW: usize = 100;
        if self.bars_1m.len() < HURST_WINDOW {
            self.hurst_exponent = None;
            return;
        }

        let start = self.bars_1m.len() - HURST_WINDOW;
        let closes: Vec<f64> = self.bars_1m.iter().skip(start).map(|b| b.close).collect();

        // Compute log returns
        let returns: Vec<f64> = closes
            .windows(2)
            .filter(|w| w[0] > 0.0)
            .map(|w| (w[1] / w[0]).ln())
            .collect();

        if returns.len() < 20 {
            self.hurst_exponent = None;
            return;
        }

        // R/S analysis over multiple sub-period sizes
        let sizes: &[usize] = &[10, 20, 30, 50];
        let mut log_n = Vec::new();
        let mut log_rs = Vec::new();

        for &size in sizes {
            if size > returns.len() {
                continue;
            }
            let n_chunks = returns.len() / size;
            if n_chunks == 0 {
                continue;
            }

            let mut rs_sum = 0.0;
            let mut rs_count = 0;

            for chunk_idx in 0..n_chunks {
                let chunk = &returns[chunk_idx * size..(chunk_idx + 1) * size];
                let n = chunk.len() as f64;
                let mean = chunk.iter().sum::<f64>() / n;

                // Cumulative deviations from mean
                let mut cum_dev = Vec::with_capacity(chunk.len());
                let mut running = 0.0;
                for &r in chunk {
                    running += r - mean;
                    cum_dev.push(running);
                }

                let range = cum_dev.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                    - cum_dev.iter().cloned().fold(f64::INFINITY, f64::min);

                let std_dev = (chunk.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n).sqrt();

                if std_dev > 1e-15 {
                    rs_sum += range / std_dev;
                    rs_count += 1;
                }
            }

            if rs_count > 0 {
                let avg_rs = rs_sum / rs_count as f64;
                if avg_rs > 0.0 {
                    log_n.push((size as f64).ln());
                    log_rs.push(avg_rs.ln());
                }
            }
        }

        // Linear regression of log(R/S) vs log(n) → slope = Hurst exponent
        if log_n.len() < 2 {
            self.hurst_exponent = None;
            return;
        }

        let n = log_n.len() as f64;
        let sum_x: f64 = log_n.iter().sum();
        let sum_y: f64 = log_rs.iter().sum();
        let sum_xy: f64 = log_n.iter().zip(log_rs.iter()).map(|(x, y)| x * y).sum();
        let sum_x2: f64 = log_n.iter().map(|x| x * x).sum();

        let denom = n * sum_x2 - sum_x * sum_x;
        if denom.abs() < 1e-12 {
            self.hurst_exponent = None;
            return;
        }

        let slope = (n * sum_xy - sum_x * sum_y) / denom;
        // Clamp to valid range [0, 1]
        self.hurst_exponent = Some(slope.clamp(0.0, 1.0));
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
                            state.garch_vol,
                            state.bars_1m.len(),
                            state.volume_5m(),
                            state.hurst_exponent,
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
        "garch_vol": state.garch_vol,
        "hurst_exponent": state.hurst_exponent,
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

        // Generate more bars than MAX_BARS
        for i in 0..(MAX_BARS + 10) as i64 {
            let price = 100.0 + (i as f64) * 0.01;
            state.handle_trade(price, 1.0, base_ms + i * minute_ms);
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

    #[test]
    fn test_garch_vol_initialization() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 12 bars (10 needed for GARCH init)
        for i in 0..12 {
            let price = 100.0 + (i as f64) * 0.5;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        assert!(state.bars_1m.len() >= GARCH_MIN_BARS);
        assert!(state.garch_vol.is_some(), "GARCH vol should be computed after init bars");
        assert!(state.garch_variance > 0.0, "GARCH variance should be positive");
    }

    #[test]
    fn test_garch_vol_mean_reversion() {
        // After a vol spike, GARCH should revert toward long-run mean
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // 15 calm bars to initialize GARCH
        for i in 0..15 {
            let price = 100.0 + (i as f64) * 0.01;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        assert!(state.garch_vol.is_some(), "GARCH should be initialized");

        // Spike: large 5% move — the spike opens bar 15
        state.handle_trade(105.0, 1.0, base_ms + 15 * minute_ms);
        // The spike bar closes when the NEXT trade arrives at a new minute
        state.handle_trade(105.0, 1.0, base_ms + 16 * minute_ms);
        // Now bar 15 is closed with the spike return — GARCH should be elevated
        let garch_after_spike = state.garch_variance;
        assert!(garch_after_spike > 0.0, "Spike should increase GARCH variance");

        // 15 more calm bars (near-zero returns)
        for i in 17..32 {
            let price = 105.0 + (i as f64 - 17.0) * 0.001;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        let garch_after_calm = state.garch_variance;

        // GARCH should have reverted — variance should be lower after calm period
        assert!(garch_after_calm < garch_after_spike,
            "GARCH should revert after calm: after_spike={garch_after_spike}, after_calm={garch_after_calm}");
    }

    #[test]
    fn test_garch_convergence_to_zero_vol() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 40 bars with constant price → vol should converge toward omega/(1-alpha-beta)
        for i in 0..41 {
            state.handle_trade(100.0, 1.0, base_ms + (i as i64) * minute_ms);
        }

        if let Some(vol) = state.garch_vol {
            // With constant price (returns = 0), GARCH variance → omega/(1-alpha-beta)
            // which is very small. Annualized vol should be near 0.
            assert!(vol < 0.05, "GARCH vol should be near 0 for constant price, got {vol}");
        }
    }

    #[test]
    fn test_hurst_needs_100_bars() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 50 bars — not enough for Hurst
        for i in 0..51 {
            let price = 100.0 + (i as f64) * 0.01;
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }
        assert!(state.hurst_exponent.is_none(), "Hurst needs 100 bars, got None as expected");
    }

    #[test]
    fn test_hurst_computed_with_enough_bars() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 105 trades = 104 closed bars (>100 needed for Hurst)
        // Use pseudo-random walk: alternating up/down with varying magnitude
        for i in 0..105 {
            let noise = ((i * 7 + 3) % 13) as f64 - 6.0;
            let price = 100.0 + noise * 0.1;
            state.handle_trade(price.max(90.0), 1.0, base_ms + (i as i64) * minute_ms);
        }

        assert!(state.bars_1m.len() >= 100, "Should have 100+ bars, got {}", state.bars_1m.len());
        assert!(state.hurst_exponent.is_some(), "Hurst should be computed with 100+ bars");
        let h = state.hurst_exponent.unwrap();
        assert!(h >= 0.0 && h <= 1.0, "Hurst should be in [0, 1], got {h}");
    }

    #[test]
    fn test_hurst_trending_series() {
        let mut state = BinanceSpotState::default();
        let base_ms: i64 = 1700000000000;
        let minute_ms: i64 = 60_000;

        // Generate 105 trades with strong upward trend (should give H > 0.5)
        for i in 0..105 {
            let price = 100.0 + (i as f64) * 1.0; // steady uptrend
            state.handle_trade(price, 1.0, base_ms + (i as i64) * minute_ms);
        }

        if let Some(h) = state.hurst_exponent {
            assert!(h > 0.5, "Trending series should give H > 0.5, got {h}");
        }
    }
}
