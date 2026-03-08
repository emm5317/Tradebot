//! Bridges KalshiWsFeed → OrderbookManager → Redis.
//!
//! Consumes WebSocket messages, maintains in-memory orderbooks,
//! and writes JSON summaries to Redis so the Python evaluator can
//! use real-time data instead of stale DB snapshots.

use std::sync::Arc;
use std::time::Duration;

use fred::clients::Client as RedisClient;
use fred::interfaces::KeysInterface;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::kalshi::orderbook::{OrderbookManager, Side};
use crate::kalshi::websocket::WsFeedMessage;

/// Run the orderbook feed consumer loop.
///
/// Receives messages from the WebSocket feed, updates the in-memory
/// orderbook, and periodically flushes summaries to Redis.
pub async fn run(
    mut rx: mpsc::Receiver<WsFeedMessage>,
    orderbooks: Arc<OrderbookManager>,
    redis: RedisClient,
    cancel: CancellationToken,
) {
    let mut flush_interval = tokio::time::interval(Duration::from_millis(500));
    let mut dirty_tickers: std::collections::HashSet<String> = std::collections::HashSet::new();

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
                    WsFeedMessage::Trade { .. } => {
                        // Trades don't change the orderbook — logged by WsFeed already
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
                flush_to_redis(&orderbooks, &redis, &dirty_tickers).await;
                dirty_tickers.clear();
            }
        }
    }
}

/// Write orderbook summaries to Redis for each dirty ticker.
///
/// Key format: `orderbook:{ticker}` with JSON matching what the
/// Python evaluator expects: mid_price, spread, best_bid, best_ask,
/// bid_depth, ask_depth.
async fn flush_to_redis(
    orderbooks: &OrderbookManager,
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

        let summary = serde_json::json!({
            "mid_price": mid.unwrap_or(0.5),
            "spread": spread.unwrap_or(0.0),
            "best_bid": best_bid,
            "best_ask": best_ask,
            "bid_depth": bid_depth,
            "ask_depth": ask_depth,
        });

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
