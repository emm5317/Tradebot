# BE-5: Execution Engine — Risk, Sizing, Orders, Positions

**Dependencies**: BE-1 (database), BE-2 (Kalshi client), BE-4 (signals via NATS)
**Blocks**: BE-6 (scanner/metrics), BE-7 (UI), BE-9 (integration)
**Language**: Rust

---

## Overview

The execution engine is the Rust core. It consumes signals, applies risk checks, sizes positions, places orders, and manages the full lifecycle through settlement. Every cent flows through this module — correctness is paramount.

---

## BE-5.1: Risk Manager

### Deliverable
`rust/src/risk/manager.rs`

### Specification

```rust
pub struct RiskManager {
    daily_loss: AtomicI64,         // cents, today's realized losses
    open_exposure: AtomicI64,      // cents, sum of open position sizes
    kill_switch: Arc<KillSwitch>,
    circuit_breaker: Arc<CircuitBreaker>,
    positions: Arc<DashMap<String, Position>>,
    config: RiskConfig,
}

pub struct RiskConfig {
    pub max_trade_size_cents: i64,
    pub max_daily_loss_cents: i64,
    pub max_positions: usize,
    pub max_exposure_cents: i64,
    pub max_positions_per_city: usize,    // NEW: correlation awareness
    pub max_positions_per_asset: usize,   // NEW
}

impl RiskManager {
    pub fn approve_order(&self, signal: &Signal) -> Result<ApprovedOrder, RiskRejection>;
}
```

### Approval pipeline (in order)
1. Kill switch active? → `Err(KillSwitchActive)`
2. Circuit breaker tripped? → `Err(CircuitBreakerTripped)`
3. Settlement time within valid window? → `Err(OutsideTimeWindow)`
4. Daily loss remaining ≥ signal size? → `Err(DailyLossExceeded)`
5. Position count < max? → `Err(PositionCapReached)`
6. **Correlation check: same city count < max_per_city?** → `Err(ConcentrationRisk)` (NEW)
7. **Correlation check: same asset class count < max_per_asset?** → `Err(ConcentrationRisk)` (NEW)
8. Open exposure + signal size ≤ max? → `Err(ExposureCapReached)`
9. Signal size ≤ max trade size? → `Err(TradeTooLarge)`
10. Return `Ok(ApprovedOrder)` with approved size

Every rejection logged at WARN with specific reason, ticker, and signal details.

### Improvements over original plan
- **Correlation-aware limits** — prevents concentrating in one city or asset class
- **Ordered pipeline** — cheapest checks first (atomics before DashMap lookups)

### Verification
- 9+ unit tests covering every rejection path
- 100% branch coverage on approval pipeline
- Correlation test: 2 Chicago positions filled, 3rd Chicago position rejected

---

## BE-5.2: Circuit Breaker

### Deliverable
`rust/src/risk/circuit_breaker.rs`

### Specification

```rust
pub struct CircuitBreaker {
    outcomes: Mutex<VecDeque<(Instant, Outcome)>>,
    tripped_until: AtomicU64,  // epoch seconds, 0 = not tripped
    config: CircuitBreakerConfig,
}

pub struct CircuitBreakerConfig {
    pub loss_threshold: usize,     // default 3
    pub window: Duration,          // default 30 minutes
    pub cooldown: Duration,        // default 60 minutes
}

impl CircuitBreaker {
    pub fn record_outcome(&self, outcome: Outcome);
    pub fn is_tripped(&self) -> bool;
    pub fn time_until_reset(&self) -> Option<Duration>;
}
```

### Verification
- 2 losses in 10 min → not tripped
- 3rd loss → tripped
- 59 min later → still tripped
- 61 min later → reset

---

## BE-5.3: Kill Switch

### Deliverable
`rust/src/risk/kill_switch.rs`

### Specification
```rust
pub struct KillSwitch {
    active: AtomicBool,
}

impl KillSwitch {
    pub fn activate(&self);     // logs ERROR, sets flag
    pub fn deactivate(&self);   // logs WARN, clears flag
    pub fn is_active(&self) -> bool;  // lock-free read
}
```

Exposed via:
- `POST /api/kill-switch` (Axum endpoint)
- Terminal UI button
- Graceful shutdown handler (auto-activates on SIGTERM)

### Verification
- Activate via API → next signal rejected
- Deactivate → signal accepted

---

## BE-5.4: Kelly Position Sizing

### Deliverable
`rust/src/execution/sizing.rs`

### Specification
```rust
pub fn compute_size(
    kelly_fraction: Decimal,
    current_balance: Decimal,
    max_trade_size: Decimal,
    kelly_multiplier: Decimal,  // 0.25 for quarter-Kelly
) -> Decimal {
    let raw = kelly_fraction * current_balance;
    let adjusted = raw * kelly_multiplier;
    let capped = adjusted.min(max_trade_size);
    // Round to nearest contract (1 cent increments on Kalshi)
    capped.round_dp(0)
}
```

All math in `rust_decimal`. **Zero floating point** in the sizing pipeline.

### Verification
- balance=$500, kelly=0.10 → $12.50 → 12 contracts
- balance=$500, kelly=0.40 → $50 → capped at $25 → 25 contracts
- balance=$50, kelly=0.05 → $0.625 → 0 contracts (below minimum)

---

## BE-5.5: Order Execution (Market + Limit)

### Deliverable
`rust/src/execution/order.rs`

### Strategy selection

```rust
pub fn select_strategy(
    signal: &Signal,
    orderbook: &Orderbook,
    minutes_remaining: f64,
) -> OrderStrategy {
    let spread = orderbook.spread();

    if minutes_remaining < 5.0 || spread <= Decimal::new(2, 2) {
        // Tight spread or running out of time — take liquidity
        OrderStrategy::Market
    } else if orderbook.depth_at_best(signal.side()) >= signal.size {
        // Enough depth — post inside the spread
        let limit_price = if signal.direction == "yes" {
            orderbook.best_bid()? + Decimal::new(1, 2)  // 1 cent above best bid
        } else {
            orderbook.best_ask()? - Decimal::new(1, 2)  // 1 cent below best ask
        };
        OrderStrategy::Limit { price: limit_price, timeout: Duration::from_secs(60) }
    } else {
        // Thin book — market order
        OrderStrategy::Market
    }
}
```

### Limit order lifecycle
1. Place limit order
2. Start 60-second timeout
3. If filled → done
4. If timeout → cancel order
5. After cancel → re-evaluate signal (may no longer be valid)
6. If still valid → market order as fallback

### Idempotency (NEW)
Generate deterministic order ID before HTTP call:
```rust
let idempotency_key = format!("{}-{}-{}", signal.ticker, signal.id, timestamp_bucket);
// Write to DB with status=pending BEFORE placing the order
db.insert_pending_order(&idempotency_key, &order_request).await?;
// Place order
let response = kalshi.place_order(order_request).await?;
// Update DB with fill details
db.update_order_filled(&idempotency_key, &response).await?;
```

On crash recovery: check for `status=pending` orders, reconcile with Kalshi.

### Verification
- Paper mode: market order fills immediately
- Paper mode: limit order placed in wide-spread market
- Limit order cancelled after 60s timeout
- Idempotency: crash after HTTP send, restart — no duplicate order

---

## BE-5.6: Position Manager

### Deliverable
`rust/src/execution/position.rs`

### Specification

```rust
pub struct PositionManager {
    positions: Arc<DashMap<String, Position>>,
    risk: Arc<RiskManager>,
    db: Arc<DbPool>,
}

pub struct Position {
    pub ticker: String,
    pub direction: String,
    pub size_cents: i64,
    pub fill_price: Decimal,
    pub model_prob_at_entry: Decimal,   // for re-evaluation
    pub opened_at: DateTime<Utc>,
    pub settlement_time: DateTime<Utc>,
    pub city: Option<String>,           // for correlation tracking
    pub asset_class: String,            // "weather" or "crypto"
}

impl PositionManager {
    pub async fn on_fill(&self, order: &FilledOrder);
    pub async fn on_settlement(&self, ticker: &str, settled_yes: bool);
    pub async fn re_evaluate(&self, ticker: &str, current_model_prob: Decimal) -> Option<ExitSignal>; // NEW
    pub async fn recover_from_db(&self);
}
```

### Exit strategy (NEW)
Continuously re-evaluate open positions:
```rust
pub async fn re_evaluate(&self, ticker: &str, current_model_prob: Decimal) -> Option<ExitSignal> {
    let position = self.positions.get(ticker)?;
    let minutes_remaining = (position.settlement_time - Utc::now()).num_minutes() as f64;

    // Current edge: how much does our model still favor us?
    let current_edge = if position.direction == "yes" {
        current_model_prob - /* current market price from orderbook */
    } else {
        /* current market price */ - current_model_prob
    };

    // Exit if edge has flipped against us with enough time to act
    if current_edge < Decimal::new(-3, 2) && minutes_remaining > 3.0 {
        return Some(ExitSignal { ticker, reason: "edge_flipped" });
    }

    None
}
```

### Crash recovery
On startup:
1. Query `orders WHERE status = 'filled' AND outcome = 'pending'`
2. For each: check if settlement has passed → fetch result from Kalshi API
3. Rebuild `DashMap` from unresolved positions
4. Reconcile `daily_loss` and `open_exposure` atomics

### Verification
- Place order → position in DashMap
- Settlement → position removed, PnL correct, daily stats updated
- Kill process, restart → positions recovered from DB
- Re-evaluate: edge flips → exit signal generated

---

## BE-5.7: Signal Consumer (NATS → Execution)

### Deliverable
`rust/src/signal/consumer.rs`

### Specification

```rust
pub struct SignalConsumer {
    nats: async_nats::jetstream::Context,
    risk: Arc<RiskManager>,
    executor: Arc<OrderExecutor>,
    position_mgr: Arc<PositionManager>,
}

impl SignalConsumer {
    pub async fn run(&self) {
        let consumer = self.nats.get_or_create_consumer("tradebot-signals", config).await?;
        let mut messages = consumer.messages().await?;

        while let Some(msg) = messages.next().await {
            let signal: Signal = simd_json::from_slice(&mut msg.payload.to_vec())?;

            // Process signal
            match self.risk.approve_order(&signal) {
                Ok(approved) => {
                    let size = compute_size(approved.kelly, balance, max_size, 0.25);
                    let strategy = select_strategy(&signal, &orderbook, minutes);
                    self.executor.execute(approved, size, strategy).await?;
                }
                Err(rejection) => {
                    tracing::warn!(ticker = signal.ticker, reason = ?rejection, "signal_rejected");
                }
            }

            msg.ack().await?;
        }
    }
}
```

### Improvements over original plan
- **NATS JetStream** — built-in consumer groups, redelivery, no manual XPENDING/XCLAIM
- **`simd-json`** deserialization — 2-3x faster than `serde_json`
- **Acknowledgment after processing** — unacked messages redeliver automatically on restart

### Verification
- Publish 5 signals from Python → all consumed and processed
- Kill Rust process mid-stream → pending messages redeliver on restart
- 100 burst signals → risk manager correctly limits to max positions

---

## Acceptance Criteria (BE-5 Complete)

- [ ] Risk manager passes all 9+ unit tests with 100% branch coverage
- [ ] Circuit breaker triggers at 3 losses in 30 min, resets after 60 min
- [ ] Kill switch activates/deactivates via API
- [ ] Kelly sizing uses `rust_decimal` only — zero floating point
- [ ] Market and limit order strategies both work in paper mode
- [ ] Idempotency keys prevent duplicate orders on crash
- [ ] Positions recovered from DB on restart
- [ ] Signals consumed from NATS with at-least-once delivery
- [ ] Correlation-aware position limits prevent concentration
