//! NATS signal consumer and order execution engine.
//!
//! Subscribes to `tradebot.signals`, deserializes incoming signals,
//! applies risk checks, and places orders via the Kalshi REST API.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::StreamExt;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::kalshi::client::KalshiClient;
use crate::kalshi::error::KalshiError;
use crate::kalshi::types::OrderRequest;

/// Signal schema matching the Python SignalSchema.
#[derive(Debug, Deserialize)]
pub struct Signal {
    pub ticker: String,
    pub signal_type: String,
    pub action: String,
    pub direction: String,
    pub model_prob: f64,
    pub market_price: f64,
    pub edge: f64,
    pub kelly_fraction: f64,
    pub minutes_remaining: f64,
    pub spread: f64,
    pub order_imbalance: f64,
}

/// Tracks open positions to prevent double-entry.
struct PositionTracker {
    positions: HashMap<String, HeldPosition>,
}

struct HeldPosition {
    direction: String,
    size_cents: i64,
    entry_price: f64,
}

impl PositionTracker {
    fn new() -> Self {
        Self {
            positions: HashMap::new(),
        }
    }

    fn has_position(&self, ticker: &str) -> bool {
        self.positions.contains_key(ticker)
    }

    fn record_entry(&mut self, ticker: &str, direction: &str, size_cents: i64, price: f64) {
        self.positions.insert(
            ticker.to_string(),
            HeldPosition {
                direction: direction.to_string(),
                size_cents,
                entry_price: price,
            },
        );
    }

    fn remove_position(&mut self, ticker: &str) {
        self.positions.remove(ticker);
    }

    fn count(&self) -> usize {
        self.positions.len()
    }

    fn total_exposure(&self) -> i64 {
        self.positions.values().map(|p| p.size_cents).sum()
    }

    fn get_position(&self, ticker: &str) -> Option<&HeldPosition> {
        self.positions.get(ticker)
    }
}

/// Daily loss tracking for circuit breaker.
struct DailyPnl {
    date: chrono::NaiveDate,
    net_pnl_cents: i64,
}

impl DailyPnl {
    fn new() -> Self {
        Self {
            date: Utc::now().date_naive(),
            net_pnl_cents: 0,
        }
    }

    fn record_pnl(&mut self, cents: i64) {
        let today = Utc::now().date_naive();
        if today != self.date {
            self.date = today;
            self.net_pnl_cents = 0;
        }
        self.net_pnl_cents += cents;
    }

    fn current_loss(&self) -> i64 {
        let today = Utc::now().date_naive();
        if today != self.date {
            return 0;
        }
        -self.net_pnl_cents.min(0)
    }
}

/// Run the execution loop: subscribe to NATS and process signals.
pub async fn run(
    config: &Config,
    nats: async_nats::Client,
    pool: sqlx::PgPool,
    kalshi: KalshiClient,
) -> Result<()> {
    let mut subscriber = nats
        .subscribe("tradebot.signals")
        .await
        .context("Failed to subscribe to tradebot.signals")?;

    info!("execution engine listening on tradebot.signals");

    let mut tracker = PositionTracker::new();
    let mut daily_pnl = DailyPnl::new();

    while let Some(msg) = subscriber.next().await {
        let start = Instant::now();

        let signal: Signal = match serde_json::from_slice(&msg.payload) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to deserialize signal");
                continue;
            }
        };

        info!(
            ticker = %signal.ticker,
            action = %signal.action,
            direction = %signal.direction,
            edge = %signal.edge,
            kelly = %signal.kelly_fraction,
            "signal received"
        );

        // Handle exit signals
        if signal.action == "exit" {
            if tracker.has_position(&signal.ticker) {
                match execute_exit(config, &kalshi, &pool, &signal, &mut tracker, &mut daily_pnl).await {
                    Ok(()) => {
                        let latency = start.elapsed();
                        info!(
                            ticker = %signal.ticker,
                            latency_ms = %latency.as_millis(),
                            "exit order executed"
                        );
                    }
                    Err(e) => error!(ticker = %signal.ticker, error = %e, "exit order failed"),
                }
            }
            continue;
        }

        // Entry signal — apply risk checks
        if let Err(reason) = check_risk(config, &signal, &tracker, &daily_pnl) {
            warn!(ticker = %signal.ticker, reason = %reason, "signal rejected by risk check");
            continue;
        }

        match execute_entry(config, &kalshi, &pool, &signal, &mut tracker).await {
            Ok(()) => {
                let latency = start.elapsed();
                info!(
                    ticker = %signal.ticker,
                    latency_ms = %latency.as_millis(),
                    "entry order executed"
                );
            }
            Err(e) => error!(ticker = %signal.ticker, error = %e, "entry order failed"),
        }
    }

    Ok(())
}

fn check_risk(
    config: &Config,
    signal: &Signal,
    tracker: &PositionTracker,
    daily_pnl: &DailyPnl,
) -> std::result::Result<(), String> {
    // No double-entry
    if tracker.has_position(&signal.ticker) {
        return Err("already holding position".into());
    }

    // Max positions
    if tracker.count() >= config.max_positions as usize {
        return Err(format!(
            "max positions reached ({}/{})",
            tracker.count(),
            config.max_positions
        ));
    }

    // Daily loss circuit breaker
    if daily_pnl.current_loss() >= config.max_daily_loss_cents as i64 {
        return Err(format!(
            "daily loss limit reached ({} >= {})",
            daily_pnl.current_loss(),
            config.max_daily_loss_cents
        ));
    }

    // Max exposure
    let size = compute_order_size(config, signal);
    if tracker.total_exposure() + size > config.max_exposure_cents as i64 {
        return Err("would exceed max exposure".into());
    }

    Ok(())
}

fn compute_order_size(config: &Config, signal: &Signal) -> i64 {
    let kelly_adjusted = signal.kelly_fraction * config.kelly_fraction_multiplier;
    let size = (kelly_adjusted * 10000.0) as i64; // Convert to cents
    size.min(config.max_trade_size_cents as i64).max(1)
}

async fn execute_entry(
    config: &Config,
    kalshi: &KalshiClient,
    pool: &sqlx::PgPool,
    signal: &Signal,
    tracker: &mut PositionTracker,
) -> Result<()> {
    let size_cents = compute_order_size(config, signal);
    let idempotency_key = format!(
        "{}-{}-{}",
        signal.ticker,
        signal.direction,
        Utc::now().timestamp_millis()
    );

    let order_req = OrderRequest {
        ticker: signal.ticker.clone(),
        action: "buy".to_string(),
        side: signal.direction.clone(),
        r#type: "market".to_string(),
        count: size_cents,
        yes_price: None,
        no_price: None,
        client_order_id: Some(idempotency_key.clone()),
    };

    if config.paper_mode {
        info!(
            ticker = %signal.ticker,
            direction = %signal.direction,
            size_cents = size_cents,
            edge = %signal.edge,
            "[PAPER] would place order"
        );
        tracker.record_entry(&signal.ticker, &signal.direction, size_cents, signal.market_price);

        // Record paper order to DB
        record_order(pool, signal, size_cents, &idempotency_key, "filled", None, 0).await?;
        return Ok(());
    }

    let start = Instant::now();
    match kalshi.place_order(order_req).await {
        Ok(resp) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            let fill_price = resp.order.yes_price.or(resp.order.no_price).map(|p| p as f64 / 100.0);
            tracker.record_entry(
                &signal.ticker,
                &signal.direction,
                size_cents,
                fill_price.unwrap_or(signal.market_price),
            );
            record_order(
                pool,
                signal,
                size_cents,
                &idempotency_key,
                "filled",
                fill_price,
                latency_ms,
            )
            .await?;
            Ok(())
        }
        Err(KalshiError::InsufficientFunds) => {
            warn!(ticker = %signal.ticker, "insufficient funds");
            record_order(pool, signal, size_cents, &idempotency_key, "failed", None, 0).await?;
            Ok(())
        }
        Err(KalshiError::MarketClosed) => {
            warn!(ticker = %signal.ticker, "market closed");
            record_order(pool, signal, size_cents, &idempotency_key, "failed", None, 0).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn execute_exit(
    config: &Config,
    kalshi: &KalshiClient,
    _pool: &sqlx::PgPool,
    signal: &Signal,
    tracker: &mut PositionTracker,
    daily_pnl: &mut DailyPnl,
) -> Result<()> {
    let idempotency_key = format!(
        "{}-exit-{}",
        signal.ticker,
        Utc::now().timestamp_millis()
    );

    // Sell the held side to close position
    let exit_side = if signal.direction == "yes" { "yes" } else { "no" };
    let position = tracker.get_position(&signal.ticker);
    let size = position.map(|p| p.size_cents).unwrap_or(1);

    let order_req = OrderRequest {
        ticker: signal.ticker.clone(),
        action: "sell".to_string(),
        side: exit_side.to_string(),
        r#type: "market".to_string(),
        count: size,
        yes_price: None,
        no_price: None,
        client_order_id: Some(idempotency_key.clone()),
    };

    if config.paper_mode {
        info!(
            ticker = %signal.ticker,
            "[PAPER] would exit position"
        );
        tracker.remove_position(&signal.ticker);
        return Ok(());
    }

    match kalshi.place_order(order_req).await {
        Ok(resp) => {
            let fill_price = resp.order.yes_price.or(resp.order.no_price).map(|p| p as f64 / 100.0);
            // Estimate PnL using fill price (simplified — real PnL comes from settlement)
            if let Some(pos) = tracker.get_position(&signal.ticker) {
                let exit_price = fill_price.unwrap_or(signal.market_price);
                let pnl_estimate = ((exit_price - pos.entry_price) * pos.size_cents as f64) as i64;
                daily_pnl.record_pnl(pnl_estimate);
            }
            tracker.remove_position(&signal.ticker);
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn record_order(
    pool: &sqlx::PgPool,
    signal: &Signal,
    size_cents: i64,
    idempotency_key: &str,
    status: &str,
    fill_price: Option<f64>,
    latency_ms: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO orders (
            idempotency_key, ticker, direction, order_type,
            size_cents, fill_price, status, latency_ms
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (idempotency_key) DO NOTHING
        "#,
    )
    .bind(idempotency_key)
    .bind(&signal.ticker)
    .bind(&signal.direction)
    .bind("market")
    .bind(size_cents)
    .bind(fill_price)
    .bind(status)
    .bind(latency_ms)
    .execute(pool)
    .await
    .context("Failed to record order")?;

    Ok(())
}
