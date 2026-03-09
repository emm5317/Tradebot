//! Bridges KalshiWsFeed → OrderbookManager → Redis.
//!
//! Consumes WebSocket messages, maintains in-memory orderbooks and trade tape,
//! and writes JSON summaries to Redis so the Python evaluator can
//! use real-time data instead of stale DB snapshots.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::feed_health::FeedHealth;
use crate::kalshi::orderbook::{OrderbookManager, Side};
use crate::kalshi::trade_tape::{TradeTape, TradeRecord};
use crate::kalshi::websocket::WsFeedMessage;

/// Snapshot of trade tape metrics extracted before async flush.
struct TapeSnapshot {
    aggr_30s: f64,
    volume_60s: f64,
    volume_300s: f64,
    last_trades: HashMap<String, Option<TradeRecord>>,
}

/// Per-ticker state from the ticker channel.
#[derive(Debug, Default, Clone)]
struct TickerState {
    yes_bid_size: Option<i64>,
    yes_ask_size: Option<i64>,
    volume: Option<i64>,
    open_interest: Option<i64>,
    prev_open_interest: Option<i64>,
    last_price_history: VecDeque<(Instant, i64)>,
    market_status: Option<String>,
}

/// Run the orderbook feed consumer loop.
///
/// Receives messages from the WebSocket feed, updates the in-memory
/// orderbook and trade tape, and periodically flushes summaries to Redis.
pub async fn run(
    mut rx: mpsc::Receiver<WsFeedMessage>,
    orderbooks: Arc<OrderbookManager>,
    trade_tape: Arc<RwLock<TradeTape>>,
    feed_health: Arc<FeedHealth>,
    redis: RedisClient,
    cancel: CancellationToken,
) {
    let mut flush_interval = tokio::time::interval(Duration::from_millis(500));
    let mut stale_check_interval = tokio::time::interval(Duration::from_secs(5));
    let mut dirty_tickers: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ticker_states: HashMap<String, TickerState> = HashMap::new();

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                info!("orderbook feed consumer shutting down");
                return;
            }
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    info!("orderbook feed channel closed");
                    return;
                };
                match msg {
                    WsFeedMessage::OrderbookSnapshot { ticker, yes_bids, yes_asks } => {
                        orderbooks.apply_snapshot(&ticker, yes_bids, yes_asks);
                        dirty_tickers.insert(ticker);
                        feed_health.record_update("kalshi_ws");
                    }
                    WsFeedMessage::OrderbookDelta { ticker, side, price_cents, delta } => {
                        let side = if side == "yes" { Side::Bid } else { Side::Ask };
                        orderbooks.apply_delta(&ticker, side, price_cents, delta);
                        dirty_tickers.insert(ticker);
                        feed_health.record_update("kalshi_ws");
                    }
                    WsFeedMessage::Trade { ticker, price_cents, count, taker_side } => {
                        {
                            let mut tape = trade_tape.write().unwrap();
                            tape.record(TradeRecord {
                                ticker: ticker.clone(),
                                price_cents,
                                count,
                                taker_side,
                                timestamp: Instant::now(),
                            });
                        }
                        dirty_tickers.insert(ticker);
                    }
                    WsFeedMessage::TickerUpdate {
                        ticker, yes_bid_size, yes_ask_size,
                        volume, open_interest, market_status,
                        last_price, ..
                    } => {
                        let state = ticker_states.entry(ticker.clone()).or_default();
                        if yes_bid_size.is_some() { state.yes_bid_size = yes_bid_size; }
                        if yes_ask_size.is_some() { state.yes_ask_size = yes_ask_size; }
                        if volume.is_some() { state.volume = volume; }
                        // 7.3d: Track OI delta
                        if let Some(oi) = open_interest {
                            state.prev_open_interest = state.open_interest;
                            state.open_interest = Some(oi);
                        }
                        // 7.3a: Track price history for momentum
                        if let Some(price) = last_price {
                            let now = Instant::now();
                            state.last_price_history.push_back((now, price));
                            // Keep only last 60 seconds
                            let cutoff = now - Duration::from_secs(60);
                            while let Some((ts, _)) = state.last_price_history.front() {
                                if *ts < cutoff {
                                    state.last_price_history.pop_front();
                                } else {
                                    break;
                                }
                            }
                        }
                        if market_status.is_some() {
                            // If market closed/settled, clear the orderbook
                            if let Some(ref status) = market_status {
                                if status == "closed" || status == "settled" {
                                    orderbooks.remove(&ticker);
                                }
                            }
                            state.market_status = market_status;
                        }

                        dirty_tickers.insert(ticker);
                    }
                    WsFeedMessage::Disconnected => {
                        warn!("ws feed disconnected, orderbook data may be stale");
                    }
                    WsFeedMessage::Reconnected => {
                        info!("ws feed reconnected");
                    }
                }
            }
            _ = flush_interval.tick() => {
                if dirty_tickers.is_empty() {
                    continue;
                }
                let tape_metrics = {
                    let tape = trade_tape.read().unwrap();
                    TapeSnapshot {
                        aggr_30s: tape.aggressiveness(Duration::from_secs(30)),
                        volume_60s: tape.recent_volume(Duration::from_secs(60)) as f64,
                        volume_300s: tape.recent_volume(Duration::from_secs(300)) as f64,
                        last_trades: dirty_tickers.iter().map(|t| (t.clone(), tape.last_trade(t).cloned())).collect(),
                    }
                };
                flush_to_redis(&orderbooks, &tape_metrics, &ticker_states, &redis, &dirty_tickers).await;
                dirty_tickers.clear();
            }
            _ = stale_check_interval.tick() => {
                check_stale_feeds(&orderbooks, &redis, &ticker_states).await;
            }
        }
    }
}

/// Write orderbook summaries to Redis for each dirty ticker.
///
/// Enhanced to include trade tape metrics and ticker channel data.
async fn flush_to_redis(
    orderbooks: &OrderbookManager,
    tape: &TapeSnapshot,
    ticker_states: &HashMap<String, TickerState>,
    redis: &RedisClient,
    tickers: &std::collections::HashSet<String>,
) {
    for ticker in tickers {
        let mid = orderbooks.mid_price(ticker).map(|d| d.to_string().parse::<f64>().unwrap_or(0.5));
        let spread = orderbooks.spread(ticker).map(|d| d.to_string().parse::<f64>().unwrap_or(0.0));
        let best_bid = orderbooks.best_bid(ticker).map(|(p, _)| p.to_string().parse::<f64>().unwrap_or(0.0));
        let best_ask = orderbooks.best_ask(ticker).map(|(p, _)| p.to_string().parse::<f64>().unwrap_or(0.0));

        let bid_depth: i64 = orderbooks.best_bid(ticker).map(|(_, s)| s).unwrap_or(0);
        let ask_depth: i64 = orderbooks.best_ask(ticker).map(|(_, s)| s).unwrap_or(0);

        // Trade tape metrics (pre-extracted from tape snapshot)
        let trade_aggr_30s = tape.aggr_30s;
        let recent_volume_60s = tape.volume_60s;
        let last_trade = tape.last_trades.get(ticker).cloned().flatten();

        // Ticker channel state
        let ts = ticker_states.get(ticker.as_str());

        let mut summary = serde_json::json!({
            "mid_price": mid.unwrap_or(0.5),
            "spread": spread.unwrap_or(0.0),
            "best_bid": best_bid,
            "best_ask": best_ask,
            "bid_depth": bid_depth,
            "ask_depth": ask_depth,
            "trade_aggr_30s": trade_aggr_30s,
            "recent_volume_60s": recent_volume_60s,
        });

        // Volume surge detection (7.3c): 60s volume vs 5-min baseline
        let volume_300s_avg = tape.volume_300s / 5.0; // per-minute average
        let volume_60s_rate = tape.volume_60s; // last minute
        let volume_surge = volume_300s_avg > 0.0 && volume_60s_rate > volume_300s_avg * 3.0;

        // Add ticker channel fields if available
        if let Some(ts) = ts {
            if let Some(s) = ts.yes_bid_size { summary["best_bid_size"] = serde_json::json!(s); }
            if let Some(s) = ts.yes_ask_size { summary["best_ask_size"] = serde_json::json!(s); }
            if let Some(ref status) = ts.market_status { summary["market_status"] = serde_json::json!(status); }
            if let Some(v) = ts.volume { summary["volume"] = serde_json::json!(v); }
            if let Some(oi) = ts.open_interest { summary["open_interest"] = serde_json::json!(oi); }

            // 7.3a: Price momentum (linear slope over last 30s)
            let momentum = compute_price_momentum(&ts.last_price_history);
            summary["price_momentum"] = serde_json::json!(momentum);

            // 7.3d: Open interest delta
            if let (Some(curr), Some(prev)) = (ts.open_interest, ts.prev_open_interest) {
                summary["oi_delta"] = serde_json::json!(curr - prev);
            }
        }

        // 7.3c: Volume surge flag
        summary["volume_surge"] = serde_json::json!(volume_surge);

        // Add last trade info
        if let Some(lt) = last_trade {
            summary["last_trade_price"] = serde_json::json!(lt.price_cents as f64 / 100.0);
            summary["last_trade_count"] = serde_json::json!(lt.count);
        }

        let key = format!("orderbook:{ticker}");
        let value = summary.to_string();

        // SET with 30s TTL — stale data auto-expires if WS disconnects
        let result: Result<(), _> = redis.set(
            &key,
            value.as_str(),
            Some(fred::types::Expiration::EX(30)),
            None,
            false,
        ).await;

        if let Err(e) = result {
            warn!(ticker, error = %e, "failed to write orderbook to redis");
        }
    }
}

/// Compute price momentum as linear regression slope over recent price history.
///
/// Returns slope in cents/second. Positive = price rising, negative = falling.
fn compute_price_momentum(history: &VecDeque<(Instant, i64)>) -> f64 {
    if history.len() < 2 {
        return 0.0;
    }
    let anchor = history.front().unwrap().0;
    let n = history.len() as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_x2 = 0.0;

    for (ts, price) in history {
        let x = ts.duration_since(anchor).as_secs_f64();
        let y = *price as f64;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_x2 += x * x;
    }

    let denom = n * sum_x2 - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    (n * sum_xy - sum_x * sum_y) / denom
}

/// Periodically check for stale feeds and write health status to Redis.
async fn check_stale_feeds(
    orderbooks: &OrderbookManager,
    redis: &RedisClient,
    ticker_states: &HashMap<String, TickerState>,
) {
    let stale_threshold = Duration::from_secs(30);

    for ticker in ticker_states.keys() {
        let is_stale = orderbooks.is_stale(ticker, stale_threshold);
        if is_stale {
            warn!(ticker, "orderbook data is stale");
            let key = format!("feed:status:{ticker}");
            let _: Result<(), _> = redis.set(
                &key,
                "stale",
                Some(fred::types::Expiration::EX(60)),
                None,
                false,
            ).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_momentum_rising() {
        let mut history = VecDeque::new();
        let base = Instant::now() - Duration::from_secs(10);
        // Prices rising: 50, 52, 54, 56, 58
        for i in 0..5 {
            history.push_back((base + Duration::from_secs(i * 2), 50 + i as i64 * 2));
        }
        let slope = compute_price_momentum(&history);
        assert!(slope > 0.0, "slope should be positive for rising prices, got {slope}");
        // ~1 cent/second
        assert!((slope - 1.0).abs() < 0.1);
    }

    #[test]
    fn test_price_momentum_falling() {
        let mut history = VecDeque::new();
        let base = Instant::now() - Duration::from_secs(10);
        for i in 0..5 {
            history.push_back((base + Duration::from_secs(i * 2), 60 - i as i64 * 2));
        }
        let slope = compute_price_momentum(&history);
        assert!(slope < 0.0, "slope should be negative for falling prices, got {slope}");
    }

    #[test]
    fn test_price_momentum_flat() {
        let mut history = VecDeque::new();
        let base = Instant::now() - Duration::from_secs(10);
        for i in 0..5 {
            history.push_back((base + Duration::from_secs(i * 2), 50));
        }
        let slope = compute_price_momentum(&history);
        assert!(slope.abs() < 0.001, "slope should be ~0 for flat prices, got {slope}");
    }

    #[test]
    fn test_price_momentum_insufficient_data() {
        let history = VecDeque::new();
        assert_eq!(compute_price_momentum(&history), 0.0);

        let mut history = VecDeque::new();
        history.push_back((Instant::now(), 50));
        assert_eq!(compute_price_momentum(&history), 0.0);
    }

    #[test]
    fn test_ticker_state_oi_delta_tracking() {
        let mut state = TickerState::default();
        assert!(state.prev_open_interest.is_none());

        // First OI update
        state.prev_open_interest = state.open_interest;
        state.open_interest = Some(100);
        assert!(state.prev_open_interest.is_none());

        // Second OI update — prev should now hold previous value
        state.prev_open_interest = state.open_interest;
        state.open_interest = Some(120);
        assert_eq!(state.prev_open_interest, Some(100));
        assert_eq!(state.open_interest.unwrap() - state.prev_open_interest.unwrap(), 20);
    }

    #[test]
    fn test_ticker_state_price_history_pruning() {
        let mut state = TickerState::default();
        let old = Instant::now() - Duration::from_secs(120);

        // Add old entries
        for i in 0..5 {
            state.last_price_history.push_back((old + Duration::from_secs(i), 50));
        }
        // Add recent entries
        let now = Instant::now();
        for i in 0..3 {
            state.last_price_history.push_back((now - Duration::from_secs(i), 55));
        }

        // Prune (mimicking what the handler does)
        let cutoff = Instant::now() - Duration::from_secs(60);
        while let Some((ts, _)) = state.last_price_history.front() {
            if *ts < cutoff {
                state.last_price_history.pop_front();
            } else {
                break;
            }
        }

        // Only recent entries should remain
        assert_eq!(state.last_price_history.len(), 3);
    }
}
