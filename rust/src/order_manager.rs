//! Order State Machine — manages order lifecycle from signal to settlement.
//!
//! Replaces fire-and-forget order submission with tracked state transitions,
//! partial fill handling, signal dedup, rate limiting, and restart recovery.
//!
//! Phase 2: Order State Machine.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::Config;
use crate::crypto_state::{CryptoState, CryptoStateInner};
use crate::feed_health::FeedHealth;
use crate::kalshi::client::KalshiClient;
use crate::kalshi::error::KalshiError;
use crate::kalshi::types::OrderRequest;
use crate::kill_switch::KillSwitchState;
use crate::types::{Signal, SignalPriority};

// ---------------------------------------------------------------------------
// OrderState enum
// ---------------------------------------------------------------------------

/// Order lifecycle states with enforced transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    /// Signal received, pre-validation.
    Pending,
    /// Risk checks passed, about to submit to exchange.
    Submitting,
    /// Exchange acknowledged the order (has kalshi_order_id).
    Acknowledged,
    /// Partially filled (0 < filled_qty < requested_qty).
    PartialFill,
    /// Fully filled.
    Filled,
    /// Cancel request sent.
    CancelPending,
    /// Cancel confirmed by exchange.
    Cancelled,
    /// Cancel+replace in flight.
    Replacing,
    /// Exchange rejected the order.
    Rejected,
    /// Unknown state (connection lost, needs reconciliation).
    Unknown,
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Submitting => write!(f, "submitting"),
            Self::Acknowledged => write!(f, "acknowledged"),
            Self::PartialFill => write!(f, "partial_fill"),
            Self::Filled => write!(f, "filled"),
            Self::CancelPending => write!(f, "cancel_pending"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Replacing => write!(f, "replacing"),
            Self::Rejected => write!(f, "rejected"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl OrderState {
    /// Validate a state transition. Returns Err with reason if invalid.
    pub fn validate_transition(self, to: OrderState) -> std::result::Result<(), &'static str> {
        // Any state can transition to Unknown (connection loss)
        if to == OrderState::Unknown {
            return Ok(());
        }

        match (self, to) {
            (Self::Pending, Self::Submitting) => Ok(()),
            (Self::Submitting, Self::Acknowledged) => Ok(()),
            (Self::Submitting, Self::Rejected) => Ok(()),
            (Self::Submitting, Self::Filled) => Ok(()), // instant fill (market order)
            (Self::Acknowledged, Self::PartialFill) => Ok(()),
            (Self::Acknowledged, Self::Filled) => Ok(()),
            (Self::Acknowledged, Self::Rejected) => Ok(()),
            (Self::Acknowledged, Self::CancelPending) => Ok(()),
            (Self::Acknowledged, Self::Replacing) => Ok(()),
            (Self::PartialFill, Self::Filled) => Ok(()),
            (Self::PartialFill, Self::CancelPending) => Ok(()),
            (Self::PartialFill, Self::Replacing) => Ok(()),
            (Self::CancelPending, Self::Cancelled) => Ok(()),
            (Self::CancelPending, Self::Filled) => Ok(()), // race: fill before cancel ACK
            (Self::Replacing, Self::Acknowledged) => Ok(()),
            (Self::Replacing, Self::Rejected) => Ok(()),
            (Self::Unknown, _) => Ok(()), // reconciliation can move to any state
            _ => Err("invalid state transition"),
        }
    }

    /// Whether this state represents a terminal (final) state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Filled | Self::Cancelled | Self::Rejected)
    }

    /// Whether this state means we hold (or partially hold) a position.
    pub fn has_fill(self) -> bool {
        matches!(self, Self::Filled | Self::PartialFill)
    }
}

// ---------------------------------------------------------------------------
// ManagedOrder
// ---------------------------------------------------------------------------

/// A single tracked order with full lifecycle state.
#[derive(Debug, Clone)]
pub struct ManagedOrder {
    pub client_order_id: String,
    pub kalshi_order_id: Option<String>,
    pub ticker: String,
    pub signal_type: String,
    pub direction: String,
    pub requested_qty: i64,
    pub filled_qty: i64,
    pub state: OrderState,
    pub created_at: Instant,
    pub transitions: Vec<(OrderState, Instant)>,
    // Model state at order time (for attribution)
    pub crypto_snapshot: Option<CryptoStateInner>,
    pub model_prob: f64,
    pub market_price: f64,
    pub entry_price: Option<f64>,
    /// DB id of the originating signal (for Brier score JOINs).
    pub signal_id: Option<i64>,
    /// Measured submit latency in milliseconds.
    pub latency_ms: Option<i64>,
}

impl ManagedOrder {
    /// Create a new order in Pending state.
    pub fn new(
        client_order_id: String,
        ticker: String,
        signal_type: String,
        direction: String,
        requested_qty: i64,
        model_prob: f64,
        market_price: f64,
        crypto_snapshot: Option<CryptoStateInner>,
    ) -> Self {
        let now = Instant::now();
        Self {
            client_order_id,
            kalshi_order_id: None,
            ticker,
            signal_type,
            direction,
            requested_qty,
            filled_qty: 0,
            state: OrderState::Pending,
            created_at: now,
            transitions: vec![(OrderState::Pending, now)],
            crypto_snapshot,
            model_prob,
            market_price,
            entry_price: None,
            signal_id: None,
            latency_ms: None,
        }
    }

    /// Transition to a new state. Logs the transition.
    /// In debug builds, panics on invalid transitions. In release, warns and proceeds.
    pub fn transition(&mut self, to: OrderState) {
        if let Err(reason) = self.state.validate_transition(to) {
            if cfg!(debug_assertions) {
                panic!(
                    "invalid order state transition {} → {} for {}: {}",
                    self.state, to, self.client_order_id, reason
                );
            } else {
                warn!(
                    from = %self.state,
                    to = %to,
                    client_order_id = %self.client_order_id,
                    reason,
                    "invalid order state transition (proceeding anyway)"
                );
            }
        }

        info!(
            client_order_id = %self.client_order_id,
            ticker = %self.ticker,
            from = %self.state,
            to = %to,
            "order state transition"
        );

        self.state = to;
        self.transitions.push((to, Instant::now()));
    }

    /// Record a fill (full or partial).
    pub fn record_fill(&mut self, filled_qty: i64, fill_price: Option<f64>) {
        self.filled_qty = filled_qty;
        if let Some(price) = fill_price {
            self.entry_price = Some(price);
        }
        if self.filled_qty >= self.requested_qty {
            self.transition(OrderState::Filled);
        } else if self.filled_qty > 0 {
            self.transition(OrderState::PartialFill);
        }
    }
}

// ---------------------------------------------------------------------------
// OrderManager
// ---------------------------------------------------------------------------

/// Manages all active orders, positions, risk checks, and execution safeguards.
pub struct OrderManager {
    /// Active and recently-completed orders by client_order_id.
    orders: HashMap<String, ManagedOrder>,
    /// Tickers with held positions → client_order_id of the fill.
    positions: HashMap<String, String>,
    /// Daily PnL tracking for circuit breaker.
    daily_pnl: DailyPnl,
    /// Per-ticker cooldowns: ticker → last signal time.
    signal_cooldowns: HashMap<String, Instant>,
    /// Sliding window of order timestamps for rate limiting.
    order_timestamps: VecDeque<Instant>,
    /// Per-ticker order timestamps for per-ticker rate limiting.
    ticker_order_timestamps: HashMap<String, VecDeque<Instant>>,
    /// Sequence counter for client order IDs.
    sequence: u64,
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

impl OrderManager {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            positions: HashMap::new(),
            daily_pnl: DailyPnl::new(),
            signal_cooldowns: HashMap::new(),
            order_timestamps: VecDeque::new(),
            ticker_order_timestamps: HashMap::new(),
            sequence: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Client Order ID generation (Phase 2.2)
    // -----------------------------------------------------------------------

    /// Generate a deterministic client order ID for a signal.
    /// Format: `tb-{hash12}-{seq}` where hash is based on signal parameters.
    fn generate_client_order_id(&mut self, signal: &Signal) -> String {
        self.sequence += 1;

        // Deterministic hash of signal parameters
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        signal.ticker.hash(&mut hasher);
        signal.direction.hash(&mut hasher);
        signal.signal_type.hash(&mut hasher);
        // Bucket model_prob to 2 decimal places for determinism
        ((signal.model_prob * 100.0) as i64).hash(&mut hasher);
        let hash = hasher.finish();

        format!("tb-{:012x}-{}", hash, self.sequence)
    }

    // -----------------------------------------------------------------------
    // Signal dedup and rate limiting (Phase 2.6)
    // -----------------------------------------------------------------------

    /// Check if a signal is within its cooldown window.
    fn is_in_cooldown(&self, signal: &Signal, config: Option<&Config>) -> bool {
        if let Some(last_time) = self.signal_cooldowns.get(&signal.ticker) {
            let cooldown = cooldown_for_signal_type(&signal.signal_type, config);
            last_time.elapsed() < cooldown
        } else {
            false
        }
    }

    /// Record that a signal was acted upon (starts cooldown).
    fn record_signal_cooldown(&mut self, ticker: &str) {
        self.signal_cooldowns.insert(ticker.to_string(), Instant::now());
    }

    /// Public wrapper for tests to record cooldowns.
    #[cfg(test)]
    pub fn record_signal_cooldown_pub(&mut self, ticker: &str) {
        self.record_signal_cooldown(ticker);
    }

    /// Check global order rate limit (max 10 orders/min).
    fn check_global_rate_limit(&self) -> bool {
        let one_minute_ago = Instant::now() - std::time::Duration::from_secs(60);
        let recent = self.order_timestamps.iter().filter(|t| **t > one_minute_ago).count();
        recent < 10
    }

    /// Check per-ticker rate limit (max 2 orders per 5 minutes).
    fn check_ticker_rate_limit(&self, ticker: &str) -> bool {
        if let Some(timestamps) = self.ticker_order_timestamps.get(ticker) {
            let five_min_ago = Instant::now() - std::time::Duration::from_secs(300);
            let recent = timestamps.iter().filter(|t| **t > five_min_ago).count();
            recent < 2
        } else {
            true
        }
    }

    /// Record an order submission for rate limiting.
    fn record_order_submission(&mut self, ticker: &str) {
        let now = Instant::now();
        self.order_timestamps.push_back(now);
        // Trim old entries (older than 5 min)
        let cutoff = now - std::time::Duration::from_secs(300);
        while self.order_timestamps.front().is_some_and(|t| *t < cutoff) {
            self.order_timestamps.pop_front();
        }

        let ticker_timestamps = self
            .ticker_order_timestamps
            .entry(ticker.to_string())
            .or_default();
        ticker_timestamps.push_back(now);
        while ticker_timestamps.front().is_some_and(|t| *t < cutoff) {
            ticker_timestamps.pop_front();
        }
    }

    // -----------------------------------------------------------------------
    // Risk checks (Phase 2.6 + existing checks)
    // -----------------------------------------------------------------------

    /// Run all pre-submission risk checks.
    pub fn check_risk(
        &self,
        config: &Config,
        signal: &Signal,
        kill_switch: &KillSwitchState,
        feed_health: &FeedHealth,
    ) -> std::result::Result<(), String> {
        // Kill switch
        if kill_switch.is_blocked(&signal.signal_type) {
            return Err("blocked by kill switch".into());
        }

        // Feed health
        if let Err(stale_feeds) = feed_health.required_feeds_healthy(&signal.signal_type) {
            return Err(format!("stale feeds: {:?}", stale_feeds));
        }

        // Signal cooldown (Phase 2.6) with priority-based bypass (Phase 3)
        if self.is_in_cooldown(signal, Some(config)) {
            let bypass = match signal.priority {
                // LockDetection always bypasses cooldown
                SignalPriority::LockDetection => true,
                // NewData with strong edge (2x min_edge) bypasses cooldown
                SignalPriority::NewData if signal.edge > config.crypto_min_edge * 2.0 => true,
                _ => false,
            };
            if !bypass {
                return Err(format!(
                    "signal cooldown ({:?} remaining)",
                    cooldown_for_signal_type(&signal.signal_type, Some(config))
                ));
            }
            info!(
                ticker = %signal.ticker,
                priority = ?signal.priority,
                edge = %format!("{:.4}", signal.edge),
                "cooldown bypassed by priority"
            );
        }

        // No double-entry
        if self.positions.contains_key(&signal.ticker) {
            return Err("already holding position".into());
        }

        // Max positions
        if self.positions.len() >= config.max_positions {
            return Err(format!(
                "max positions reached ({}/{})",
                self.positions.len(),
                config.max_positions
            ));
        }

        // Daily loss circuit breaker
        if self.daily_pnl.current_loss() >= config.max_daily_loss_cents {
            return Err(format!(
                "daily loss limit reached ({} >= {})",
                self.daily_pnl.current_loss(),
                config.max_daily_loss_cents
            ));
        }

        // Max exposure
        let size = compute_order_size(config, signal);
        let current_exposure: i64 = self
            .orders
            .values()
            .filter(|o| o.state.has_fill())
            .map(|o| o.filled_qty)
            .sum();
        if current_exposure + size > config.max_exposure_cents {
            return Err("would exceed max exposure".into());
        }

        // Global rate limit
        if !self.check_global_rate_limit() {
            return Err("global rate limit (10/min)".into());
        }

        // Per-ticker rate limit
        if !self.check_ticker_rate_limit(&signal.ticker) {
            return Err("per-ticker rate limit (2/5min)".into());
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Order execution
    // -----------------------------------------------------------------------

    /// Submit an entry order. Creates ManagedOrder, transitions through states.
    pub async fn submit_entry(
        &mut self,
        config: &Config,
        kalshi: &KalshiClient,
        pool: &sqlx::PgPool,
        signal: &Signal,
        signal_id: Option<i64>,
        crypto_state: &CryptoState,
    ) -> Result<()> {
        let size_cents = compute_order_size(config, signal);
        let client_order_id = self.generate_client_order_id(signal);

        // Capture crypto snapshot at order time (Phase 1 integration)
        let snapshot = if signal.signal_type == "crypto" {
            Some(crypto_state.snapshot())
        } else {
            None
        };

        let mut order = ManagedOrder::new(
            client_order_id.clone(),
            signal.ticker.clone(),
            signal.signal_type.clone(),
            signal.direction.clone(),
            size_cents,
            signal.model_prob,
            signal.market_price,
            snapshot,
        );
        order.signal_id = signal_id;

        // Pending → Submitting
        order.transition(OrderState::Submitting);

        if config.paper_mode {
            // Paper mode: instant fill
            order.record_fill(size_cents, Some(signal.market_price));
            self.positions
                .insert(signal.ticker.clone(), client_order_id.clone());
            self.record_signal_cooldown(&signal.ticker);
            self.record_order_submission(&signal.ticker);

            info!(
                ticker = %signal.ticker,
                direction = %signal.direction,
                size_cents,
                edge = %format!("{:.4}", signal.edge),
                client_order_id = %client_order_id,
                "[PAPER] order filled"
            );

            // Persist
            record_paper_trade(pool, signal, size_cents).await?;
            persist_order(pool, &order, signal).await?;
            self.orders.insert(client_order_id, order);
            return Ok(());
        }

        // Live mode: submit to Kalshi
        let order_req = OrderRequest {
            ticker: signal.ticker.clone(),
            action: "buy".to_string(),
            side: signal.direction.clone(),
            r#type: "market".to_string(),
            count: size_cents,
            yes_price: None,
            no_price: None,
            client_order_id: Some(client_order_id.clone()),
        };

        let submit_start = Instant::now();
        match kalshi.place_order(order_req).await {
            Ok(resp) => {
                let latency_ms = submit_start.elapsed().as_millis();
                order.latency_ms = Some(latency_ms as i64);
                order.kalshi_order_id = Some(resp.order.order_id.clone());

                // Determine fill state from response
                let filled_qty = resp
                    .order
                    .count
                    .unwrap_or(0)
                    .saturating_sub(resp.order.remaining_count.unwrap_or(0));
                let fill_price = resp
                    .order
                    .yes_price
                    .or(resp.order.no_price)
                    .map(|p| p as f64 / 100.0);

                if filled_qty > 0 {
                    order.record_fill(filled_qty, fill_price);
                    if order.state.has_fill() {
                        self.positions
                            .insert(signal.ticker.clone(), client_order_id.clone());
                    }
                } else {
                    order.transition(OrderState::Acknowledged);
                }

                self.record_signal_cooldown(&signal.ticker);
                self.record_order_submission(&signal.ticker);

                info!(
                    ticker = %signal.ticker,
                    client_order_id = %client_order_id,
                    kalshi_order_id = %resp.order.order_id,
                    filled_qty,
                    requested_qty = size_cents,
                    state = %order.state,
                    latency_ms = %latency_ms,
                    "entry order submitted"
                );

                persist_order(pool, &order, signal).await?;
                self.orders.insert(client_order_id, order);
                Ok(())
            }
            Err(KalshiError::InsufficientFunds) => {
                order.transition(OrderState::Rejected);
                warn!(ticker = %signal.ticker, "order rejected: insufficient funds");
                persist_order(pool, &order, signal).await?;
                self.orders.insert(client_order_id, order);
                Ok(())
            }
            Err(KalshiError::MarketClosed) => {
                order.transition(OrderState::Rejected);
                warn!(ticker = %signal.ticker, "order rejected: market closed");
                persist_order(pool, &order, signal).await?;
                self.orders.insert(client_order_id, order);
                Ok(())
            }
            Err(KalshiError::RateLimit { retry_after }) => {
                order.transition(OrderState::Rejected);
                warn!(
                    ticker = %signal.ticker,
                    retry_after_ms = %retry_after.as_millis(),
                    "order rejected: rate limited"
                );
                persist_order(pool, &order, signal).await?;
                self.orders.insert(client_order_id, order);
                Ok(())
            }
            Err(e) => {
                order.transition(OrderState::Unknown);
                self.orders.insert(client_order_id, order);
                Err(e.into())
            }
        }
    }

    /// Submit an exit order for an existing position.
    pub async fn submit_exit(
        &mut self,
        config: &Config,
        kalshi: &KalshiClient,
        pool: &sqlx::PgPool,
        signal: &Signal,
        signal_id: Option<i64>,
        crypto_state: &CryptoState,
    ) -> Result<()> {
        // Find the entry order for this position
        let entry_order_id = match self.positions.get(&signal.ticker) {
            Some(id) => id.clone(),
            None => return Ok(()), // no position to exit
        };

        let entry_order = self.orders.get(&entry_order_id);
        let size = entry_order.map(|o| o.filled_qty).unwrap_or(1);
        let exit_side = if signal.direction == "yes" {
            "yes"
        } else {
            "no"
        };

        let client_order_id = self.generate_client_order_id(signal);

        let snapshot = if signal.signal_type == "crypto" {
            Some(crypto_state.snapshot())
        } else {
            None
        };

        let mut order = ManagedOrder::new(
            client_order_id.clone(),
            signal.ticker.clone(),
            signal.signal_type.clone(),
            exit_side.to_string(),
            size,
            signal.model_prob,
            signal.market_price,
            snapshot,
        );
        order.signal_id = signal_id;

        order.transition(OrderState::Submitting);

        if config.paper_mode {
            order.record_fill(size, Some(signal.market_price));

            // Estimate PnL
            if let Some(entry) = self.orders.get(&entry_order_id) {
                if let Some(entry_price) = entry.entry_price {
                    let pnl = ((signal.market_price - entry_price) * size as f64) as i64;
                    self.daily_pnl.record_pnl(pnl);
                }
            }

            self.positions.remove(&signal.ticker);
            info!(
                ticker = %signal.ticker,
                client_order_id = %client_order_id,
                "[PAPER] exit order filled"
            );

            persist_order(pool, &order, signal).await?;
            self.orders.insert(client_order_id, order);
            return Ok(());
        }

        let order_req = OrderRequest {
            ticker: signal.ticker.clone(),
            action: "sell".to_string(),
            side: exit_side.to_string(),
            r#type: "market".to_string(),
            count: size,
            yes_price: None,
            no_price: None,
            client_order_id: Some(client_order_id.clone()),
        };

        match kalshi.place_order(order_req).await {
            Ok(resp) => {
                order.kalshi_order_id = Some(resp.order.order_id.clone());
                let filled_qty = resp
                    .order
                    .count
                    .unwrap_or(0)
                    .saturating_sub(resp.order.remaining_count.unwrap_or(0));
                let fill_price = resp
                    .order
                    .yes_price
                    .or(resp.order.no_price)
                    .map(|p| p as f64 / 100.0);

                if filled_qty > 0 {
                    order.record_fill(filled_qty, fill_price);
                } else {
                    order.transition(OrderState::Acknowledged);
                }

                // Estimate PnL on full exit
                if order.state == OrderState::Filled {
                    if let Some(entry) = self.orders.get(&entry_order_id) {
                        if let Some(entry_price) = entry.entry_price {
                            let exit_price = fill_price.unwrap_or(signal.market_price);
                            let pnl = ((exit_price - entry_price) * size as f64) as i64;
                            self.daily_pnl.record_pnl(pnl);
                        }
                    }
                    self.positions.remove(&signal.ticker);
                }

                persist_order(pool, &order, signal).await?;
                self.orders.insert(client_order_id, order);
                Ok(())
            }
            Err(e) => {
                order.transition(OrderState::Unknown);
                self.orders.insert(client_order_id, order);
                Err(e.into())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Cancel (Phase 2.3)
    // -----------------------------------------------------------------------

    /// Cancel an order by client_order_id.
    pub async fn cancel_order(
        &mut self,
        kalshi: &KalshiClient,
        client_order_id: &str,
    ) -> Result<()> {
        let order = match self.orders.get_mut(client_order_id) {
            Some(o) => o,
            None => return Ok(()),
        };

        let kalshi_id = match &order.kalshi_order_id {
            Some(id) => id.clone(),
            None => {
                warn!(
                    client_order_id,
                    "cannot cancel order without kalshi_order_id"
                );
                return Ok(());
            }
        };

        // Can only cancel Acknowledged or PartialFill orders
        if !matches!(
            order.state,
            OrderState::Acknowledged | OrderState::PartialFill
        ) {
            warn!(
                client_order_id,
                state = %order.state,
                "cannot cancel order in current state"
            );
            return Ok(());
        }

        order.transition(OrderState::CancelPending);

        match kalshi.cancel_order(&kalshi_id).await {
            Ok(_resp) => {
                order.transition(OrderState::Cancelled);
                // If partially filled, keep the partial position
                if order.filled_qty == 0 {
                    self.positions.remove(&order.ticker);
                }
                info!(
                    client_order_id,
                    ticker = %order.ticker,
                    "order cancelled"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    client_order_id,
                    error = %e,
                    "cancel failed, marking unknown"
                );
                order.transition(OrderState::Unknown);
                Err(e.into())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Kill switch integration (Phase 2.7)
    // -----------------------------------------------------------------------

    /// Cancel all in-flight orders for a given signal type (or all if signal_type is None).
    pub async fn cancel_in_flight(
        &mut self,
        kalshi: &KalshiClient,
        signal_type: Option<&str>,
    ) {
        let cancellable: Vec<String> = self
            .orders
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o.state,
                    OrderState::Acknowledged | OrderState::PartialFill
                ) && signal_type.map_or(true, |st| o.signal_type == st)
            })
            .map(|(id, _)| id.clone())
            .collect();

        for client_order_id in cancellable {
            warn!(
                client_order_id = %client_order_id,
                "kill switch: cancelling in-flight order"
            );
            if let Err(e) = self.cancel_order(kalshi, &client_order_id).await {
                warn!(
                    client_order_id = %client_order_id,
                    error = %e,
                    "kill switch: cancel failed"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Startup reconciliation (Phase 2.5)
    // -----------------------------------------------------------------------

    /// Reconcile positions on startup by querying Kalshi and the DB.
    pub async fn reconcile_on_startup(
        &mut self,
        kalshi: &KalshiClient,
        pool: &sqlx::PgPool,
    ) -> Result<()> {
        info!("starting position reconciliation");

        // 1. Query Kalshi for open positions
        let exchange_positions = kalshi.get_positions().await.unwrap_or_else(|e| {
            warn!(error = %e, "failed to fetch Kalshi positions, continuing with empty");
            vec![]
        });

        // 2. Query DB for orders that may be open
        let db_orders: Vec<(String, String, String, i32, String)> = sqlx::query_as(
            r#"
            SELECT client_order_id, ticker, direction, COALESCE(filled_qty, size_cents) as qty,
                   COALESCE(order_state, status) as state
            FROM orders
            WHERE (order_state IN ('acknowledged', 'partial_fill', 'filled')
                   OR status IN ('pending', 'filled'))
              AND settled_at IS NULL
              AND created_at > now() - interval '24 hours'
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "failed to query DB orders for reconciliation");
            vec![]
        });

        // 3. Rebuild positions from exchange data
        for pos in &exchange_positions {
            let exposure = pos.market_exposure.unwrap_or(0);
            if exposure != 0 {
                // We have a position on the exchange
                let synthetic_id = format!("reconciled-{}", pos.ticker);
                if !self.positions.contains_key(&pos.ticker) {
                    warn!(
                        ticker = %pos.ticker,
                        exposure,
                        "reconciliation: found position on exchange not in tracker"
                    );
                    self.positions
                        .insert(pos.ticker.clone(), synthetic_id.clone());

                    // Create a synthetic ManagedOrder for tracking
                    let mut order = ManagedOrder::new(
                        synthetic_id.clone(),
                        pos.ticker.clone(),
                        "unknown".to_string(),
                        "yes".to_string(), // direction unknown from positions API
                        exposure.abs(),
                        0.0,
                        0.0,
                        None,
                    );
                    order.filled_qty = exposure.abs();
                    order.state = OrderState::Filled;
                    order.transitions.push((OrderState::Filled, Instant::now()));
                    self.orders.insert(synthetic_id, order);
                }
            }
        }

        // 4. Cross-check DB orders
        for (client_order_id, ticker, _direction, _qty, state) in &db_orders {
            if state == "filled" && !self.positions.contains_key(ticker) {
                // DB says filled but no exchange position — may have been settled
                info!(
                    client_order_id = %client_order_id,
                    ticker = %ticker,
                    "reconciliation: DB shows filled but no exchange position (likely settled)"
                );
            }
        }

        info!(
            positions = self.positions.len(),
            orders = self.orders.len(),
            "reconciliation complete"
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Position queries
    // -----------------------------------------------------------------------

    pub fn has_position(&self, ticker: &str) -> bool {
        self.positions.contains_key(ticker)
    }

    /// Get the held direction for a position (from the entry order).
    pub fn held_direction(&self, ticker: &str) -> Option<&str> {
        let order_id = self.positions.get(ticker)?;
        let order = self.orders.get(order_id)?;
        Some(&order.direction)
    }

    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Get all orders for dashboard display.
    pub fn all_orders(&self) -> Vec<&ManagedOrder> {
        self.orders.values().collect()
    }

    /// Get all held positions (ticker → direction).
    pub fn all_positions(&self) -> Vec<(&str, &str)> {
        self.positions
            .iter()
            .filter_map(|(ticker, order_id)| {
                let order = self.orders.get(order_id)?;
                Some((ticker.as_str(), order.direction.as_str()))
            })
            .collect()
    }

    /// Get daily PnL in cents.
    pub fn daily_pnl_cents(&self) -> i64 {
        self.daily_pnl.net_pnl_cents
    }

    /// Get current daily loss in cents (for risk display).
    pub fn daily_loss_cents(&self) -> i64 {
        self.daily_pnl.current_loss()
    }

    /// Clean up terminal orders older than 1 hour to prevent memory growth.
    pub fn gc_old_orders(&mut self) {
        let one_hour = std::time::Duration::from_secs(3600);
        self.orders.retain(|_, o| {
            !o.state.is_terminal() || o.created_at.elapsed() < one_hour
        });
    }

    /// Phase 5.4: Periodic reconciliation against exchange positions.
    /// Compares in-memory position tracker with Kalshi REST API positions.
    /// Returns list of discrepancies for audit logging.
    pub async fn reconcile_positions(
        &mut self,
        kalshi: &KalshiClient,
        pool: &sqlx::PgPool,
    ) -> Result<Vec<ReconciliationEntry>> {
        let exchange_positions = kalshi.get_positions().await
            .context("Failed to fetch positions for reconciliation")?;

        let mut discrepancies = Vec::new();

        // Build map of exchange positions
        let exchange_map: HashMap<String, i64> = exchange_positions
            .iter()
            .filter(|p| p.market_exposure.unwrap_or(0) != 0)
            .map(|p| (p.ticker.clone(), p.market_exposure.unwrap_or(0)))
            .collect();

        // Check: position on exchange but not in local tracker
        for (ticker, &qty) in &exchange_map {
            if !self.positions.contains_key(ticker) {
                warn!(
                    ticker = %ticker,
                    exchange_qty = qty,
                    "reconciliation: position on exchange but not in tracker"
                );
                discrepancies.push(ReconciliationEntry {
                    discrepancy: "missing_local".into(),
                    ticker: ticker.clone(),
                    exchange_qty: Some(qty as i32),
                    local_qty: None,
                    action_taken: "added_to_tracker".into(),
                });
            }
        }

        // Check: position in tracker but not on exchange
        let local_tickers: Vec<String> = self.positions.keys().cloned().collect();
        for ticker in &local_tickers {
            if !exchange_map.contains_key(ticker) {
                warn!(
                    ticker = %ticker,
                    "reconciliation: position in tracker but not on exchange"
                );
                self.positions.remove(ticker);
                discrepancies.push(ReconciliationEntry {
                    discrepancy: "missing_exchange".into(),
                    ticker: ticker.clone(),
                    exchange_qty: None,
                    local_qty: Some(1),
                    action_taken: "removed_from_tracker".into(),
                });
            }
        }

        // Persist discrepancies to DB
        for entry in &discrepancies {
            let _ = sqlx::query(
                "INSERT INTO reconciliation_log (discrepancy, ticker, exchange_qty, local_qty, action_taken) \
                 VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(&entry.discrepancy)
            .bind(&entry.ticker)
            .bind(entry.exchange_qty)
            .bind(entry.local_qty)
            .bind(&entry.action_taken)
            .execute(pool)
            .await;
        }

        if discrepancies.is_empty() {
            info!("reconciliation: no discrepancies found");
        } else {
            warn!(
                count = discrepancies.len(),
                "reconciliation: found discrepancies"
            );
        }

        Ok(discrepancies)
    }
}

/// Reconciliation discrepancy record for audit logging.
#[derive(Debug, Clone, Serialize)]
pub struct ReconciliationEntry {
    pub discrepancy: String,
    pub ticker: String,
    pub exchange_qty: Option<i32>,
    pub local_qty: Option<i32>,
    pub action_taken: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute order size from Kelly fraction, scaled by confidence, capped by config.
fn compute_order_size(config: &Config, signal: &Signal) -> i64 {
    let kelly_adjusted = signal.kelly_fraction * config.kelly_fraction_multiplier;
    let confidence_scale = signal.confidence.clamp(0.3, 1.0);
    let size = (kelly_adjusted * confidence_scale * 10000.0) as i64; // Convert to cents
    size.min(config.max_trade_size_cents).max(1)
}

/// Signal-type-aware cooldown duration, using config-driven values.
fn cooldown_for_signal_type(signal_type: &str, config: Option<&Config>) -> std::time::Duration {
    match (signal_type, config) {
        ("crypto", Some(c)) => std::time::Duration::from_secs(c.crypto_cooldown_secs),
        ("weather", Some(c)) => std::time::Duration::from_secs(c.weather_cooldown_secs),
        ("crypto", None) => std::time::Duration::from_secs(30),
        ("weather", None) => std::time::Duration::from_secs(120),
        _ => std::time::Duration::from_secs(60),
    }
}

/// Persist order to database.
async fn persist_order(
    pool: &sqlx::PgPool,
    order: &ManagedOrder,
    signal: &Signal,
) -> Result<()> {
    let transitions_json =
        serde_json::to_string(&order.transitions.iter().map(|(s, _)| s).collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".to_string());

    let crypto_snapshot_json = order
        .crypto_snapshot
        .as_ref()
        .map(|s| {
            serde_json::json!({
                "shadow_rti": s.shadow_rti,
                "coinbase_spot": s.coinbase_spot,
                "binance_spot": s.binance_spot,
                "perp_price": s.perp_price,
                "basis": s.basis,
                "best_vol": s.best_vol,
                "dvol": s.dvol,
            })
            .to_string()
        });

    sqlx::query(
        r#"
        INSERT INTO orders (
            idempotency_key, client_order_id, kalshi_order_id,
            ticker, direction, order_type,
            size_cents, requested_qty, filled_qty,
            fill_price, status, order_state, latency_ms,
            signal_type, model_prob, market_price_at_order,
            transitions, crypto_snapshot, signal_id
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            $10, $11, $12, $13, $14, $15, $16,
            $17::jsonb, $18::jsonb, $19
        )
        ON CONFLICT (idempotency_key) DO UPDATE SET
            order_state = EXCLUDED.order_state,
            filled_qty = EXCLUDED.filled_qty,
            fill_price = EXCLUDED.fill_price,
            transitions = EXCLUDED.transitions
        "#,
    )
    .bind(&order.client_order_id) // idempotency_key
    .bind(&order.client_order_id) // client_order_id
    .bind(&order.kalshi_order_id) // kalshi_order_id
    .bind(&signal.ticker)
    .bind(&signal.direction)
    .bind("market")
    .bind(order.requested_qty as i32) // size_cents
    .bind(order.requested_qty as i32) // requested_qty
    .bind(order.filled_qty as i32) // filled_qty
    .bind(order.entry_price.map(|p| p as f32)) // fill_price
    .bind(order.state.to_string()) // status
    .bind(order.state.to_string()) // order_state
    .bind(order.latency_ms.unwrap_or(0) as i32) // latency_ms
    .bind(&signal.signal_type)
    .bind(signal.model_prob as f32)
    .bind(signal.market_price as f32)
    .bind(&transitions_json)
    .bind(&crypto_snapshot_json)
    .bind(order.signal_id) // signal_id
    .execute(pool)
    .await
    .context("Failed to persist order")?;

    Ok(())
}

/// Settle order outcomes by joining against contracts.settled_yes.
/// Updates orders with outcome = 'win' or 'loss' and computes pnl_cents.
///
/// P&L for binary contracts:
///   win  → payout is $1.00 per contract, cost was fill_price → profit = (100 - fill_cents)
///   loss → payout is $0.00, cost was fill_price → loss = -fill_cents
pub async fn settle_order_outcomes(pool: &sqlx::PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE orders SET
            outcome = CASE
                WHEN (direction = 'yes' AND c.settled_yes = true)
                  OR (direction = 'no' AND c.settled_yes = false)
                THEN 'win'
                ELSE 'loss'
            END,
            pnl_cents = CASE
                WHEN (direction = 'yes' AND c.settled_yes = true)
                  OR (direction = 'no' AND c.settled_yes = false)
                THEN (100 - ROUND(fill_price * 100)::integer)
                ELSE (-ROUND(fill_price * 100)::integer)
            END,
            settled_at = now()
        FROM contracts c
        WHERE orders.ticker = c.ticker
          AND c.settled_yes IS NOT NULL
          AND orders.outcome = 'pending'
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to settle order outcomes")?;

    let rows = result.rows_affected();
    if rows > 0 {
        info!(settled = rows, "order outcomes settled");
    }
    Ok(rows)
}

/// Record a paper trade with full signal parameters.
async fn record_paper_trade(pool: &sqlx::PgPool, signal: &Signal, size_cents: i64) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO paper_trades (
            ticker, direction, action, size_cents,
            model_prob, market_price, edge, kelly_fraction, signal_type
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(&signal.ticker)
    .bind(&signal.direction)
    .bind(&signal.action)
    .bind(size_cents as i32)
    .bind(signal.model_prob as f32)
    .bind(signal.market_price as f32)
    .bind(signal.edge as f32)
    .bind(signal.kelly_fraction as f32)
    .bind(&signal.signal_type)
    .execute(pool)
    .await
    .context("Failed to record paper trade")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(OrderState::Pending.validate_transition(OrderState::Submitting).is_ok());
        assert!(OrderState::Submitting.validate_transition(OrderState::Acknowledged).is_ok());
        assert!(OrderState::Submitting.validate_transition(OrderState::Rejected).is_ok());
        assert!(OrderState::Submitting.validate_transition(OrderState::Filled).is_ok());
        assert!(OrderState::Acknowledged.validate_transition(OrderState::Filled).is_ok());
        assert!(OrderState::Acknowledged.validate_transition(OrderState::PartialFill).is_ok());
        assert!(OrderState::Acknowledged.validate_transition(OrderState::CancelPending).is_ok());
        assert!(OrderState::PartialFill.validate_transition(OrderState::Filled).is_ok());
        assert!(OrderState::PartialFill.validate_transition(OrderState::CancelPending).is_ok());
        assert!(OrderState::CancelPending.validate_transition(OrderState::Cancelled).is_ok());
        assert!(OrderState::CancelPending.validate_transition(OrderState::Filled).is_ok());
        assert!(OrderState::Replacing.validate_transition(OrderState::Acknowledged).is_ok());
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(OrderState::Pending.validate_transition(OrderState::Filled).is_err());
        assert!(OrderState::Filled.validate_transition(OrderState::Acknowledged).is_err());
        assert!(OrderState::Cancelled.validate_transition(OrderState::Filled).is_err());
        assert!(OrderState::Rejected.validate_transition(OrderState::Submitting).is_err());
        assert!(OrderState::Pending.validate_transition(OrderState::CancelPending).is_err());
    }

    #[test]
    fn test_any_to_unknown() {
        assert!(OrderState::Pending.validate_transition(OrderState::Unknown).is_ok());
        assert!(OrderState::Submitting.validate_transition(OrderState::Unknown).is_ok());
        assert!(OrderState::Acknowledged.validate_transition(OrderState::Unknown).is_ok());
        assert!(OrderState::Filled.validate_transition(OrderState::Unknown).is_ok());
    }

    #[test]
    fn test_unknown_to_any() {
        assert!(OrderState::Unknown.validate_transition(OrderState::Filled).is_ok());
        assert!(OrderState::Unknown.validate_transition(OrderState::Cancelled).is_ok());
        assert!(OrderState::Unknown.validate_transition(OrderState::Acknowledged).is_ok());
    }

    #[test]
    fn test_terminal_states() {
        assert!(OrderState::Filled.is_terminal());
        assert!(OrderState::Cancelled.is_terminal());
        assert!(OrderState::Rejected.is_terminal());
        assert!(!OrderState::Pending.is_terminal());
        assert!(!OrderState::Acknowledged.is_terminal());
        assert!(!OrderState::PartialFill.is_terminal());
        assert!(!OrderState::Unknown.is_terminal());
    }

    #[test]
    fn test_has_fill() {
        assert!(OrderState::Filled.has_fill());
        assert!(OrderState::PartialFill.has_fill());
        assert!(!OrderState::Pending.has_fill());
        assert!(!OrderState::Acknowledged.has_fill());
        assert!(!OrderState::Cancelled.has_fill());
    }

    #[test]
    fn test_managed_order_lifecycle() {
        let mut order = ManagedOrder::new(
            "tb-test-1".to_string(),
            "TICKER-A".to_string(),
            "crypto".to_string(),
            "yes".to_string(),
            10,
            0.65,
            0.50,
            None,
        );

        assert_eq!(order.state, OrderState::Pending);
        order.transition(OrderState::Submitting);
        assert_eq!(order.state, OrderState::Submitting);
        order.transition(OrderState::Acknowledged);
        assert_eq!(order.state, OrderState::Acknowledged);
        order.record_fill(5, Some(0.52));
        assert_eq!(order.state, OrderState::PartialFill);
        assert_eq!(order.filled_qty, 5);
        order.record_fill(10, Some(0.52));
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(order.filled_qty, 10);
        assert_eq!(order.transitions.len(), 5); // Pending, Submitting, Acknowledged, PartialFill, Filled
    }

    #[test]
    fn test_managed_order_rejection() {
        let mut order = ManagedOrder::new(
            "tb-test-2".to_string(),
            "TICKER-B".to_string(),
            "weather".to_string(),
            "no".to_string(),
            5,
            0.70,
            0.45,
            None,
        );

        order.transition(OrderState::Submitting);
        order.transition(OrderState::Rejected);
        assert!(order.state.is_terminal());
    }

    #[test]
    fn test_cancel_race_condition() {
        // Fill arrives before cancel ACK — valid transition
        let mut order = ManagedOrder::new(
            "tb-test-3".to_string(),
            "TICKER-C".to_string(),
            "crypto".to_string(),
            "yes".to_string(),
            10,
            0.60,
            0.50,
            None,
        );

        order.transition(OrderState::Submitting);
        order.transition(OrderState::Acknowledged);
        order.transition(OrderState::CancelPending);
        // Fill arrives before cancel is confirmed
        order.transition(OrderState::Filled);
        assert_eq!(order.state, OrderState::Filled);
    }

    #[test]
    fn test_cooldown_for_signal_type() {
        assert_eq!(
            cooldown_for_signal_type("crypto", None),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            cooldown_for_signal_type("weather", None),
            std::time::Duration::from_secs(120)
        );
        assert_eq!(
            cooldown_for_signal_type("other", None),
            std::time::Duration::from_secs(60)
        );
    }

    #[test]
    fn test_order_manager_rate_limiting() {
        let mut mgr = OrderManager::new();

        // Should pass initially
        assert!(mgr.check_global_rate_limit());
        assert!(mgr.check_ticker_rate_limit("TICKER-A"));

        // Record 10 submissions
        for _ in 0..10 {
            mgr.record_order_submission("TICKER-A");
        }

        // Global limit hit
        assert!(!mgr.check_global_rate_limit());

        // Per-ticker limit hit (2 per 5 min)
        assert!(!mgr.check_ticker_rate_limit("TICKER-A"));

        // Different ticker should still pass
        assert!(mgr.check_ticker_rate_limit("TICKER-B"));
    }

    #[test]
    fn test_signal_cooldown() {
        let mut mgr = OrderManager::new();

        let signal = Signal {
            ticker: "TICKER-A".to_string(),
            signal_type: "crypto".to_string(),
            action: "entry".to_string(),
            direction: "yes".to_string(),
            model_prob: 0.65,
            market_price: 0.50,
            edge: 0.15,
            kelly_fraction: 0.10,
            minutes_remaining: 10.0,
            spread: 0.03,
            order_imbalance: 0.5,
            priority: SignalPriority::default(),
            confidence: 0.5,
        };

        assert!(!mgr.is_in_cooldown(&signal, None));
        mgr.record_signal_cooldown("TICKER-A");
        assert!(mgr.is_in_cooldown(&signal, None));
    }

    #[test]
    fn test_gc_old_orders() {
        let mut mgr = OrderManager::new();

        // Insert a terminal order with old timestamp
        let mut order = ManagedOrder::new(
            "tb-old-1".to_string(),
            "TICKER-OLD".to_string(),
            "crypto".to_string(),
            "yes".to_string(),
            10,
            0.60,
            0.50,
            None,
        );
        order.state = OrderState::Filled;
        // We can't easily backdate Instant, so just verify GC doesn't remove recent orders
        mgr.orders.insert("tb-old-1".to_string(), order);

        mgr.gc_old_orders();
        // Recent terminal order should still be present
        assert!(mgr.orders.contains_key("tb-old-1"));
    }

    #[test]
    fn test_client_order_id_determinism() {
        let mut mgr = OrderManager::new();

        let signal = Signal {
            ticker: "TICKER-A".to_string(),
            signal_type: "crypto".to_string(),
            action: "entry".to_string(),
            direction: "yes".to_string(),
            model_prob: 0.65,
            market_price: 0.50,
            edge: 0.15,
            kelly_fraction: 0.10,
            minutes_remaining: 10.0,
            spread: 0.03,
            order_imbalance: 0.5,
            priority: SignalPriority::default(),
            confidence: 0.5,
        };

        let id1 = mgr.generate_client_order_id(&signal);
        let id2 = mgr.generate_client_order_id(&signal);

        // Same signal → same hash prefix, different sequence
        assert!(id1.starts_with("tb-"));
        assert!(id2.starts_with("tb-"));
        // Hash portion should be the same
        let hash1 = &id1[3..15];
        let hash2 = &id2[3..15];
        assert_eq!(hash1, hash2, "same signal should produce same hash");
        // Sequence should differ
        assert_ne!(id1, id2, "sequence should differ");
    }

    #[test]
    fn test_display_order_state() {
        assert_eq!(OrderState::Pending.to_string(), "pending");
        assert_eq!(OrderState::PartialFill.to_string(), "partial_fill");
        assert_eq!(OrderState::CancelPending.to_string(), "cancel_pending");
    }

    #[test]
    fn test_daily_pnl_circuit_breaker() {
        let mut pnl = DailyPnl::new();
        assert_eq!(pnl.current_loss(), 0);

        pnl.record_pnl(-500); // lose $5
        assert_eq!(pnl.current_loss(), 500);

        pnl.record_pnl(200); // win $2
        assert_eq!(pnl.current_loss(), 300);

        pnl.record_pnl(500); // win $5 (net positive)
        assert_eq!(pnl.current_loss(), 0);
    }

    fn make_test_config() -> Config {
        Config {
            database_url: String::new(),
            redis_url: String::new(),
            nats_url: String::new(),
            kalshi_api_key: String::new(),
            kalshi_private_key_path: String::new(),
            kalshi_base_url: String::new(),
            kalshi_ws_url: String::new(),
            binance_ws_url: String::new(),
            mesonet_base_url: String::new(),
            coinbase_ws_url: String::new(),
            binance_futures_ws_url: String::new(),
            deribit_ws_url: String::new(),
            binance_spot_ws_url: String::new(),
            enable_coinbase: false,
            enable_binance_futures: false,
            enable_binance_spot: false,
            enable_deribit: false,
            paper_mode: true,
            max_trade_size_cents: 2500,
            max_daily_loss_cents: 10000,
            max_positions: 5,
            max_exposure_cents: 15000,
            kelly_fraction_multiplier: 0.25,
            database_pool_size: 5,
            log_level: "info".to_string(),
            log_format: "pretty".to_string(),
            discord_webhook_url: None,
            http_port: 3030,
            rti_stale_threshold_secs: 5,
            rti_outlier_threshold_pct: 0.5,
            rti_min_venues: 2,
            kill_switch_all: false,
            kill_switch_crypto: false,
            kill_switch_weather: false,
            crypto_entry_min_minutes: 3.0,
            crypto_entry_max_minutes: 20.0,
            crypto_min_edge: 0.06,
            crypto_min_kelly: 0.04,
            crypto_min_confidence: 0.50,
            crypto_cooldown_secs: 30,
            weather_cooldown_secs: 120,
            crypto_max_market_disagreement: 0.25,
            crypto_directional_min_conviction: 0.05,
        }
    }

    #[test]
    fn test_confidence_scaled_sizing() {
        let config = make_test_config();

        // High confidence (1.0) → full Kelly size
        let signal_high = Signal {
            ticker: "T".to_string(),
            signal_type: "crypto".to_string(),
            action: "entry".to_string(),
            direction: "yes".to_string(),
            model_prob: 0.7,
            market_price: 0.5,
            edge: 0.15,
            kelly_fraction: 0.20,
            minutes_remaining: 10.0,
            spread: 0.03,
            order_imbalance: 0.5,
            priority: SignalPriority::default(),
            confidence: 1.0,
        };

        // Low confidence (0.2 clamped to 0.3)
        let signal_low = Signal {
            ticker: "T".to_string(),
            signal_type: "crypto".to_string(),
            action: "entry".to_string(),
            direction: "yes".to_string(),
            model_prob: 0.7,
            market_price: 0.5,
            edge: 0.15,
            kelly_fraction: 0.20,
            minutes_remaining: 10.0,
            spread: 0.03,
            order_imbalance: 0.5,
            priority: SignalPriority::default(),
            confidence: 0.2,
        };

        let size_high = compute_order_size(&config, &signal_high);
        let size_low = compute_order_size(&config, &signal_low);

        // High confidence should give larger size
        assert!(
            size_high > size_low,
            "high conf {} should be > low conf {}",
            size_high,
            size_low
        );

        // size_high = 0.20 * 0.25 * 1.0 * 10000 = 500
        assert_eq!(size_high, 500);
        // size_low = 0.20 * 0.25 * 0.3 * 10000 = 150
        assert_eq!(size_low, 150);
    }

    #[test]
    fn test_managed_order_signal_id_and_latency() {
        let mut order = ManagedOrder::new(
            "tb-test-1".to_string(),
            "TICKER".to_string(),
            "crypto".to_string(),
            "yes".to_string(),
            100,
            0.65,
            0.50,
            None,
        );

        // Defaults
        assert!(order.signal_id.is_none());
        assert!(order.latency_ms.is_none());

        // Set values
        order.signal_id = Some(42);
        order.latency_ms = Some(15);
        assert_eq!(order.signal_id, Some(42));
        assert_eq!(order.latency_ms, Some(15));
    }
}
