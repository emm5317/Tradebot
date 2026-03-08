//! Bridges KalshiWsFeed → OrderbookManager → Redis.
//!
//! Consumes WebSocket messages, maintains in-memory orderbooks and trade tape,
//! and writes JSON summaries to Redis so the Python evaluator can
//! use real-time data instead of stale DB snapshots.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::kalshi::orderbook::{OrderbookManager, Side};
use crate::kalshi::trade_tape::{TradeTape, TradeRecord};
use crate::kalshi::websocket::WsFeedMessage;

/// Snapshot of trade tape metrics extracted before async flush.
struct TapeSnapshot {
    aggr_30s: f64,
    volume_60s: f64,
    last_trades: HashMap<String, Option<TradeRecord>>,
}

/// Per-ticker state from the ticker channel.
#[derive(Debug, Default, Clone)]
struct TickerState {
    yes_bid_size: Option<i64>,
    yes_ask_size: Option<i64>,
    volume: Option<i64>,
    open_interest: Option<i64>,
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
                    }
                    WsFeedMessage::OrderbookDelta { ticker, side, price_cents, delta } => {
                        let side = if side == "yes" { Side::Bid } else { Side::Ask };
                        orderbooks.apply_delta(&ticker, side, price_cents, delta);
                        dirty_tickers.insert(ticker);
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
                        ..
                    } => {
                        let state = ticker_states.entry(ticker.clone()).or_default();
                        if yes_bid_size.is_some() { state.yes_bid_size = yes_bid_size; }
                        if yes_ask_size.is_some() { state.yes_ask_size = yes_ask_size; }
                        if volume.is_some() { state.volume = volume; }
                        if open_interest.is_some() { state.open_interest = open_interest; }
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

        // Add ticker channel fields if available
        if let Some(ts) = ts {
            if let Some(s) = ts.yes_bid_size { summary["best_bid_size"] = serde_json::json!(s); }
            if let Some(s) = ts.yes_ask_size { summary["best_ask_size"] = serde_json::json!(s); }
            if let Some(ref status) = ts.market_status { summary["market_status"] = serde_json::json!(status); }
            if let Some(v) = ts.volume { summary["volume"] = serde_json::json!(v); }
            if let Some(oi) = ts.open_interest { summary["open_interest"] = serde_json::json!(oi); }
        }

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
