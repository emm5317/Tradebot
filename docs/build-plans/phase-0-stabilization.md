# Phase 0 — Immediate Stabilization

**Timeline:** Weeks 1–2
**Risk:** HIGH
**Goal:** Eliminate the most dangerous gaps before any live money flows

---

## 0.1 Binance Spot Feed Migration (Python → Rust)

### Problem
The Python `BinanceFeed` in `python/data/binance_ws.py` runs inside the evaluator daemon, adding ~200ms latency and creating a single point of failure for BTC spot price and volatility. All other exchange feeds already live in Rust.

### Implementation

**New file:** `rust/src/feeds/binance_spot.rs`

Follow the established feed pattern (`binance_futures.rs`):
- `BinanceSpotFeed` struct with `ws_url: String`, `cancel: CancellationToken`
- `run()` with exponential-backoff reconnect loop
- `connect_and_stream()` for WS lifecycle
- `parse_binance_spot_message()` for trade parsing
- `flush_binance_spot_state()` to Redis every 500ms

**State struct — `BinanceSpotState`:**
```rust
struct BinanceSpotState {
    spot_price: f64,
    // 1-min OHLC bar tracking
    current_bar_minute: i64,
    current_open: f64,
    current_high: f64,
    current_low: f64,
    current_close: f64,
    current_volume: f64,
    // Circular buffer of closed bars (max 60)
    bars_1m: VecDeque<OhlcBar>,
    // Volatility
    realized_vol_30m: Option<f64>,
    ewma_vol_30m: Option<f64>,
    ewma_variance: f64,
}
```

**Volatility computation ported from Python:**
- EWMA lambda = 0.94 (RiskMetrics standard)
- 1-min bar accumulation, close bar on minute boundary rollover
- Realized vol: stdev of 30 log-returns × sqrt(525_600) annualization
- EWMA vol: `var_t = 0.94 * var_{t-1} + 0.06 * r_t^2`, then sqrt + annualize
- Initialize EWMA from simple variance of first 10 bars

**Redis key:** `crypto:binance_spot` with 30s TTL
```json
{
  "spot_price": 95000.50,
  "realized_vol_30m": 0.65,
  "ewma_vol_30m": 0.70,
  "bars_count": 42,
  "updated_at": "2026-03-08T12:00:00Z"
}
```

**Config changes (`rust/src/config.rs`):**
- Add `enable_binance_spot: bool` (default: false)
- Add `binance_spot_ws_url: String` (default: `wss://stream.binance.com:9443/ws/btcusdt@trade`)

**Wire into `main.rs`:**
```rust
if config.enable_binance_spot {
    let feed = feeds::binance_spot::BinanceSpotFeed::new(...);
    tokio::spawn(async move { feed.run(redis_clone).await });
}
```

**Python evaluator changes (`python/evaluator/daemon.py`):**
- Remove `BinanceFeed` import and `self.btc_feed` initialization
- Remove `self.btc_feed.connect()` from `asyncio.gather`
- Add `_fetch_btc_state_from_redis()` method that reads `crypto:binance_spot`
- Replace `btc_state = self.btc_feed.get_state()` with Redis fetch

**Delete:** `python/data/binance_ws.py`

### Tests

**Rust tests (in `binance_spot.rs`):**
1. `test_parse_trade` — price extraction from trade message
2. `test_bar_rollover` — bar closes on minute boundary
3. `test_ohlc_tracking` — high/low/close tracked correctly
4. `test_realized_vol_computation` — matches Python output for same data
5. `test_ewma_vol_computation` — EWMA convergence behavior
6. `test_ewma_initialization` — initializes from simple variance when >=10 bars
7. `test_insufficient_bars` — returns None when <31 bars for realized vol

**Python tests:** `test_binance.py` tests remain but test the removed module — delete or convert to integration tests that verify Redis key format.

### Rollback
If the Rust feed fails, re-enable `BinanceFeed` in Python by reverting the evaluator daemon changes.

---

## 0.2 Kill Switches via Axum HTTP

### Problem
No way to halt trading without killing the process. Need per-strategy and global kill switches accessible via HTTP.

### Implementation

**New dependency:** `axum = "0.8"` in `Cargo.toml`

**New file:** `rust/src/kill_switch.rs`
```rust
pub struct KillSwitchState {
    pub kill_all: AtomicBool,
    pub kill_crypto: AtomicBool,
    pub kill_weather: AtomicBool,
}
```

**Axum routes:**
- `GET /kill-switch` → returns JSON of all switch states
- `POST /kill-switch` → accepts `{"switch": "all"|"crypto"|"weather", "active": bool}`
- `GET /health` → basic health check (useful for monitoring)

**Integration with execution.rs:**
- `run()` accepts `Arc<KillSwitchState>`
- Before order submission: `if kill_switch.is_blocked(&signal.signal_type) { warn + skip }`
- `is_blocked()` checks `kill_all || kill_{strategy}` based on signal_type

**NATS notification:** Publish to `tradebot.kill_switch` on state change for audit trail

**Redis visibility:** Write `feed:status:kill_switch` for dashboard consumption

**Config:** Add `KILL_SWITCH_ALL`, `KILL_SWITCH_CRYPTO`, `KILL_SWITCH_WEATHER` env vars (default: false)

### Wire into main.rs
```rust
let kill_switch = Arc::new(KillSwitchState::from_config(&config));
// Pass to execution
execution::run(&config, nats, pool, kalshi, Arc::clone(&kill_switch)).await
// Spawn Axum
let app = kill_switch::router(Arc::clone(&kill_switch));
let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.http_port)).await?;
tokio::spawn(axum::serve(listener, app).into_future());
```

### Tests
1. `test_kill_all_blocks_everything` — both crypto and weather blocked
2. `test_kill_crypto_only` — weather still passes
3. `test_kill_weather_only` — crypto still passes
4. `test_toggle_via_post` — state changes correctly
5. `test_default_state` — all switches off by default

---

## 0.3 Paper Mode Runtime Guard

### Problem
Paper mode is a config bool with no structural enforcement. Accidental live trading with bad config is catastrophic.

### Implementation

**Startup assertion in `main.rs`:**
```rust
if !config.paper_mode {
    tracing::warn!("🔴 LIVE TRADING MODE — PAPER_MODE=false");
    tracing::warn!("🔴 Orders will be submitted to {} with real money", config.kalshi_base_url);
} else {
    tracing::info!("📄 Paper trading mode active — no real orders");
}
```

**New migration:** `migrations/013_paper_trades.sql`
```sql
CREATE TABLE paper_trades (
    id              BIGSERIAL PRIMARY KEY,
    ticker          TEXT NOT NULL,
    direction       TEXT NOT NULL,
    action          TEXT NOT NULL,
    size_cents      INTEGER NOT NULL,
    model_prob      REAL NOT NULL,
    market_price    REAL NOT NULL,
    edge            REAL NOT NULL,
    kelly_fraction  REAL NOT NULL,
    signal_type     TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_paper_trades_created ON paper_trades(created_at);
CREATE INDEX idx_paper_trades_ticker ON paper_trades(ticker);
```

**Execution engine changes:**
- Paper orders write to `paper_trades` table with full signal parameters
- Separate `record_paper_trade()` function for clean separation

### Tests
1. Verify paper trade record captures all signal fields
2. Verify WARN log emitted when `paper_mode = false`

---

## 0.4 Feed Health Baselines

### Problem
If a feed goes stale (WS disconnect, exchange outage), the execution engine may act on stale data. No staleness detection exists.

### Implementation

**New file:** `rust/src/feed_health.rs`
```rust
pub struct FeedHealth {
    last_update: DashMap<String, Instant>,
    thresholds: HashMap<String, Duration>,
}
```

**Default staleness thresholds:**
| Feed | Threshold |
|------|-----------|
| `kalshi_ws` | 5s |
| `coinbase` | 2s |
| `binance_spot` | 2s |
| `binance_futures` | 2s |
| `deribit` | 10s |

**API:**
- `record_update(feed_name: &str)` — called by each feed on every message
- `is_healthy(feed_name: &str) -> bool` — checks `now - last_update < threshold`
- `required_feeds_healthy(signal_type: &str) -> Result<(), Vec<String>>` — returns list of stale feeds

**Feed requirements by strategy:**
- `crypto`: `binance_spot` + `coinbase` (or `binance_futures`)
- `weather`: `kalshi_ws` only

**Integration:**
- Each feed calls `feed_health.record_update()` on every parsed message
- `execution.rs` calls `feed_health.required_feeds_healthy(&signal.signal_type)` before order submission
- Stale feeds → reject signal with logged reason

**Redis visibility:** Write `feed:status:{feed_name}` with `{"healthy": true, "last_update": "...", "threshold_ms": 2000}` for dashboard

### Tests
1. `test_healthy_when_recent` — feed reported <threshold ago → healthy
2. `test_stale_when_no_update` — no update recorded → stale
3. `test_stale_after_threshold` — feed reported >threshold ago → stale
4. `test_required_feeds_crypto` — validates correct feeds checked for crypto
5. `test_required_feeds_weather` — validates correct feeds checked for weather

---

## Verification Checklist

- [x] `cargo build` compiles cleanly with new axum dep
- [x] `cargo test` passes all existing + new Rust tests
- [x] `pytest` passes all Python tests (evaluator daemon reads from Redis)
- [x] Binance spot feed writes to `crypto:binance_spot` Redis key
- [x] Kill switch GET/POST endpoints respond correctly
- [x] Paper mode logs prominently at startup
- [x] Stale feed detection blocks execution
- [x] `python/data/binance_ws.py` deleted

**Status: Complete**
