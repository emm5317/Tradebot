# Backend Build Plan — Analysis & Improvements

Critical analysis of `BACKEND_BUILD_PLAN.md` with actionable improvements organized by impact on **profitability**, **latency**, **reliability**, and **developer velocity**.

---

## 1. High-Impact Improvements

### 1.1 Replace Redis Streams with NATS JetStream

**Current plan**: Redis Streams for Python→Rust signal transport.

**Problem**: Redis Streams are adequate but add operational complexity (consumer groups, XPENDING/XCLAIM retry logic, manual acknowledgment). Redis is single-threaded — under burst load, signal delivery competes with orderbook cache reads.

**Recommendation**: Use [NATS](https://nats.io/) with JetStream for inter-process messaging.

| Factor | Redis Streams | NATS JetStream |
|--------|--------------|----------------|
| Latency (p99) | ~200μs | ~100μs |
| At-least-once delivery | Manual (XACK) | Built-in |
| Consumer groups | Manual setup | Native |
| Replay / rewind | XRANGE (manual) | Built-in stream replay |
| Operational overhead | Moderate | Low (single binary) |
| Rust crate | `redis` | `async-nats` (excellent) |
| Python client | `redis-py` | `nats-py` |

NATS also supports request/reply, which simplifies the scanner→Python→signal pipeline (BE-6.1 sends a request, Python replies with a signal or "no signal"). This eliminates the two-stream design.

**Keep Redis** for: orderbook cache, position state, rate limiting. It's excellent as a fast KV store — just not as a message broker.

**Impact**: Lower latency, simpler retry logic, fewer moving parts in the critical path.

### 1.2 Use TimescaleDB Instead of Plain PostgreSQL

**Current plan**: PostgreSQL 17 for everything including time-series observations and market snapshots.

**Problem**: Tables `observations`, `market_snapshots`, and `calibration` are append-heavy time-series data. Plain PostgreSQL requires manual partitioning, manual retention policies, and index bloat management. Queries like "get all observations for KORD in the last 30 minutes" become slow as tables grow past millions of rows.

**Recommendation**: Use [TimescaleDB](https://www.timescale.com/) (PostgreSQL extension, not a separate database).

Benefits:
- **Hypertables**: automatic time-based partitioning — insert performance stays constant
- **Continuous aggregates**: materialized views that auto-refresh (perfect for calibration rolling windows)
- **Compression**: 10-20x compression on cold data, transparent to queries
- **Retention policies**: `SELECT remove_retention_policy('observations', INTERVAL '1 year')` — no cron jobs
- **Compatible**: it's still PostgreSQL — `sqlx` works unchanged, migrations work unchanged

Apply hypertables to: `observations`, `market_snapshots`, `calibration`, `signals`.

**Impact**: Backtest queries 5-50x faster. No manual partition management. Data retention handled automatically.

### 1.3 Add Ensemble Weather Model (Not Just Physics)

**Current plan**: Single Gaussian diffusion model (`physics.py`) with one σ parameter.

**Problem**: A single-parameter physics model is fragile. Temperature doesn't follow a simple random walk — it has:
- **Diurnal patterns** (warming during the day, cooling at night)
- **Regime changes** (frontal passages cause non-Gaussian jumps)
- **Station-specific biases** (urban heat island, elevation, coastal effects)
- **Seasonal σ variation** (summer convection vs winter inversions)

**Recommendation**: Keep the physics model as one input, but add:

1. **Climatological prior** — what does the historical distribution of temperature at this station/hour/month say? Use the `observations` table from the collector. Package: `scipy.stats` for distribution fitting.

2. **Trend extrapolation** — fit a short-term linear trend to the last 60 minutes of observations. If temperature has been rising 0.5°F/10min for the last hour, that's information the random walk ignores.

3. **Ensemble combiner** — weighted average of physics model, climatological prior, and trend extrapolation. Weights calibrated by the feedback loop (BE-8.3).

```python
# Ensemble: weighted combination
p_physics = compute_weather_probability(...)       # existing
p_climo = climatological_probability(...)           # new
p_trend = trend_extrapolation_probability(...)      # new

# Weights from calibration (start equal, adjust)
p_ensemble = w1*p_physics + w2*p_climo + w3*p_trend
```

Package: [scikit-learn](https://scikit-learn.org/) `IsotonicRegression` for calibrating ensemble probabilities (Platt scaling / isotonic regression is standard in probabilistic forecasting).

**Impact**: Significantly better calibration out of the box. The single biggest lever for profitability.

### 1.4 Add Market Microstructure Features to Signal

**Current plan**: Edge = |model_prob - market_price|. Binary decision.

**Problem**: This ignores market microstructure signals that predict whether the edge is real or noise:
- **Spread width** — wide spreads mean the market is uncertain, your edge may be real
- **Order imbalance** — if bids >> asks, the market is about to move your way (or already has)
- **Time-weighted price** — a price that just moved vs. one that's been stable carry different information
- **Volume at best** — thin best levels mean your signal is more likely to move the market

**Recommendation**: Add microstructure features to the signal evaluation:

```python
def evaluate_signal(contract, observation, orderbook_state):
    model_prob = ensemble_probability(...)
    market_price = orderbook_state.mid_price
    edge = abs(model_prob - market_price)

    # Microstructure adjustments
    spread = orderbook_state.spread
    if spread > 0.10:
        edge *= 0.8  # wide spread = less confident in mid-price accuracy

    imbalance = orderbook_state.bid_volume / (orderbook_state.bid_volume + orderbook_state.ask_volume)
    # imbalance > 0.7 buying pressure, < 0.3 selling pressure

    # Only trade if edge survives spread cost
    effective_edge = edge - (spread / 2)  # account for half-spread cost
    ...
```

The orderbook state is already available (BE-2.4). This is about using it in signal evaluation, not just execution.

**Impact**: Fewer false signals, better trade selection, reduced slippage.

### 1.5 Add Dry-Run Simulation Mode

**Current plan**: Paper mode uses Kalshi demo API. No way to simulate without any API calls.

**Problem**: During development and backtesting, you want to run the full pipeline without touching any external API. The demo API has rate limits and may not always be available.

**Recommendation**: Add a `DRY_RUN=true` mode that:
- Replaces the Kalshi client with a mock that simulates fills based on orderbook state
- Replaces data fetchers with replay from the `observations` and `market_snapshots` tables
- Runs the full pipeline (scanner → signal → risk → execution → settlement) against historical data in accelerated time

This is different from backtesting (BE-8) — backtesting is offline analysis. Dry-run is the real pipeline running against replayed data, which catches integration bugs that backtests miss.

**Impact**: Faster development iteration. Catch integration bugs before paper trading.

---

## 2. Latency Improvements

### 2.1 Use `fred` Instead of `redis-rs` for Redis

**Current plan**: Implies `redis-rs` for Rust Redis client.

**Recommendation**: Use [`fred`](https://github.com/amalber/fred.rs) — a higher-level Redis client built on Tokio with:
- Connection pooling built-in
- Automatic pipeline batching (groups multiple commands into fewer round-trips)
- Cluster support (future-proofing)
- Better error handling and reconnection

If sticking with Redis for KV operations (even if NATS handles messaging), `fred` is meaningfully faster under concurrent access than raw `redis-rs`.

### 2.2 Use `tokio-tungstenite` for Kalshi WS (confirmed)

**Current plan**: Implies `tokio-tungstenite` for WebSocket.

**Recommendation**: Stick with [`tokio-tungstenite`](https://github.com/snapview/tokio-tungstenite) — the battle-tested choice. While `fastwebsockets` claims fewer allocations, benchmarks show `tokio-tungstenite` actually outperforms it in aggregate tests (7603ms vs 10141ms). More importantly, `fastwebsockets` has been flagged as **unsound and not thread-safe** with non-strict WebSocket spec compliance — a dealbreaker for production trading. Recent `tokio-tungstenite` versions (post-0.26.2) include significant performance improvements.

For the Binance Python feed, use the [`websockets`](https://websockets.readthedocs.io/) library (not `aiohttp` WS) — it's purpose-built, handles ping/pong correctly, and has better backpressure handling.

Consider [`papaya`](https://github.com/ibraheem-ca/papaya) over `dashmap` for the orderbook `OrderbookManager` — it's lock-free (no tail latency spikes from sharded locks), which matters for trading where worst-case latency kills edge.

### 2.3 Use `simd-json` for Signal Deserialization

**Current plan**: Mentions simd-json on the hot path but doesn't specify how.

**Recommendation**: Use [`simd-json`](https://github.com/simd-lite/simd-json) crate directly for deserializing signals from NATS/Redis. For the signal struct (small JSON, ~200 bytes), simd-json is 2-3x faster than `serde_json`. Integration:

```rust
// In signal consumer
let signal: Signal = simd_json::from_slice(&mut payload)?;
```

Note: `simd-json` requires `&mut [u8]` (it modifies the input buffer). Plan buffer management accordingly.

### 2.4 Pre-compute Settlement Windows

**Current plan**: Scanner checks every market update against settlement times.

**Recommendation**: Maintain a `BTreeMap<Instant, Vec<Ticker>>` sorted by settlement time. On startup, populate from Kalshi markets. Then use `range()` to efficiently query "what settles in the next 18 minutes" — O(log n) instead of scanning all markets on every tick.

When new markets appear on the WebSocket, insert into the BTree. When markets settle, remove. This makes the scanner essentially zero-cost even with thousands of active markets.

---

## 3. Reliability Improvements

### 3.1 Add Idempotency Keys to Orders

**Current plan**: No mention of idempotency.

**Problem**: If the Rust process crashes after sending an order but before recording the fill, a restart could place a duplicate order. Kalshi supports idempotency via client-generated order IDs.

**Recommendation**: Generate a deterministic order ID from `(ticker, signal_id, timestamp_bucket)` before placing any order. Store the intended order in the DB with status `pending` before the HTTP call. On restart, check for pending orders and reconcile against Kalshi's order history.

### 3.2 Add Health Check Endpoint

**Current plan**: No health check beyond "does the binary start."

**Recommendation**: Add `GET /api/health` that returns:
```json
{
  "status": "healthy",
  "kalshi_ws": "connected",
  "binance_ws": "connected",
  "redis": "connected",
  "postgres": "connected",
  "last_signal_age_seconds": 45,
  "last_order_age_seconds": 120
}
```

Status degrades to `degraded` if any feed is disconnected, `unhealthy` if DB is down. This is essential for monitoring when running unattended.

### 3.3 Add WAL-Based State Recovery

**Current plan**: Crash recovery reads from PostgreSQL.

**Problem**: Writing every state change to PostgreSQL on the hot path adds latency. But in-memory-only state is lost on crash.

**Recommendation**: Use a write-ahead log (WAL) pattern:
1. Before any state mutation, append the operation to a local append-only file (or SQLite WAL)
2. Periodically (every 5s), flush accumulated state to PostgreSQL in a batch
3. On crash recovery, replay the WAL file to reconstruct state since last PostgreSQL flush

This gives durability without adding PostgreSQL latency to every order. Crate: [`sled`](https://github.com/spacejam/sled) for embedded storage, or simply append to a file with `bincode` serialization.

### 3.4 Add Alert/Notification System

**Current plan**: Logs to stdout/file. No alerting.

**Recommendation**: Add lightweight alerting for critical events:
- Kill switch activated
- Circuit breaker tripped
- Daily loss limit hit (80% and 100%)
- Feed disconnection > 60 seconds
- Calibration drift detected

Options (low overhead):
- **Desktop notifications**: [`notify-rust`](https://github.com/h4llow3En/notify-rust) crate for system tray notifications
- **Webhook**: POST to a Discord/Slack webhook (single HTTP call, async)
- **Email**: via SMTP (for end-of-day summaries)

Start with Discord webhook — it's a single POST request, easily configured, and provides mobile notifications for free.

---

## 4. Profitability Improvements

### 4.1 Add Dynamic σ by Station and Time of Day

**Current plan**: Single global σ = 0.3°F per 10 minutes.

**Problem**: Temperature volatility varies dramatically:
- **Coastal stations** (KJFK) have lower σ due to maritime moderation
- **Continental stations** (KDEN) have higher σ, especially with altitude
- **Time of day**: σ is highest in afternoon (convective mixing), lowest at night (stable boundary layer)
- **Season**: Summer thunderstorm days have σ 3-5x normal

**Recommendation**: Build a σ lookup table from historical data:

```python
# σ per (station, hour_of_day, month)
sigma_table = compute_historical_sigma(observations_df)
sigma = sigma_table.get((station, hour, month), default=0.3)
```

This is a simple groupby on the `observations` table — no ML required. The collector daemon (BE-3.3) builds the data passively.

**Impact**: 10-20% improvement in calibration. Directly translates to larger edges and fewer bad trades.

### 4.2 Add Contract Selection Scoring

**Current plan**: Evaluate every contract in the 18-minute window equally.

**Problem**: Not all contracts are equally profitable. Contracts with wider spreads cost more to trade. Contracts in liquid markets have less edge. Contracts near threshold have higher uncertainty (more edge but more risk).

**Recommendation**: Score and rank contracts before evaluation:

```
score = expected_edge * liquidity_discount * spread_penalty
```

Where:
- `expected_edge` = estimated from historical calibration for similar setups
- `liquidity_discount` = 1.0 for thin books (more edge), 0.7 for thick books
- `spread_penalty` = 1.0 - (spread / 0.20)

Process contracts in score order. Stop when position limit is reached. This ensures the best opportunities get filled first.

### 4.3 Add Exit Strategy (Sell Before Settlement)

**Current plan**: Hold every position to settlement. Binary outcome.

**Problem**: Sometimes the edge disappears after entry (temperature moves against you, or the market corrects to your model's price). Holding a losing position to settlement when you could exit at a smaller loss reduces overall profitability.

**Recommendation**: Continuously re-evaluate open positions:

```
current_edge = model_prob - market_price  (for YES positions)

if current_edge < -0.03 and minutes_remaining > 3:
    # Edge has flipped against us — exit at market
    sell_position(ticker)
```

This requires the signal engine to re-evaluate open positions, not just new opportunities. The position manager (BE-5.6) should feed open positions back to the scanner for continuous monitoring.

**Impact**: Reduces average loss size. Even a small improvement in average loss has outsized impact on Sharpe ratio.

### 4.4 Implement Correlation-Aware Position Limits

**Current plan**: Simple position count cap (max 4).

**Problem**: 4 positions in Chicago, NYC, Denver, and Dallas are diversified. 4 positions in 4 different Chicago temperature contracts are concentrated — if the temperature moves, all 4 lose simultaneously.

**Recommendation**: Track correlation groups:
- Same city = correlated (count as 1 diversified position)
- Same asset class (all weather, all crypto) = partially correlated
- Cross-asset = uncorrelated

Adjust position limits by correlation:
```
max_positions_per_city = 2
max_positions_per_asset_class = 3
max_total_positions = 5  # up from 4, since diversified
```

---

## 5. Developer Velocity Improvements

### 5.1 Use `just` (Justfile) Extensively

**Current plan**: Mentions `justfile` but doesn't detail recipes.

**Recommendation**: Define all common operations:

```justfile
# Infrastructure
db-up:        docker compose -f docker/docker-compose.yml up -d
db-down:      docker compose -f docker/docker-compose.yml down
migrate:      sqlx migrate run --source migrations/
reset-db:     sqlx database drop && sqlx database create && just migrate

# Development
dev:          cargo run -- --paper
collector:    python -m collector.daemon
signals:      python -m signals.main

# Testing
test-rust:    cargo test
test-python:  pytest python/ -v
test:         just test-rust && just test-python
backtest:     python -m backtest.runner

# Diagnostics
logs:         tail -f /tmp/tradebot.log | jq .
status:       curl -s localhost:3000/api/status | jq .
health:       curl -s localhost:3000/api/health | jq .
```

### 5.2 Use `pydantic` for All Python Data Models

**Current plan**: Uses dataclasses.

**Recommendation**: Use [`pydantic`](https://docs.pydantic.dev/) v2 instead of dataclasses for:
- Automatic validation on construction (catches bad data from APIs immediately)
- JSON serialization/deserialization built-in
- Schema generation (useful for documenting the Redis signal format)
- `.model_dump()` for database inserts

Pydantic v2 is Rust-backed and nearly as fast as raw dataclasses. The validation it provides catches entire categories of bugs at the boundary.

### 5.3 Use `uv` for Python Package Management

**Recommendation**: Use [`uv`](https://github.com/astral-sh/uv) instead of pip/pipenv/poetry:
- 10-100x faster package resolution and installation
- Built-in virtual environment management
- `uv.lock` for reproducible builds
- Single tool replaces pip, pip-tools, virtualenv, and pyproject.toml management

### 5.4 Add `sqlx` Compile-Time Query Checking

**Current plan**: Mentions `sqlx prepare` for offline mode.

**Recommendation**: Enable `sqlx` compile-time query checking from day one:
1. Run `cargo sqlx prepare` after every migration change
2. Check in the `.sqlx/` directory
3. CI verifies queries match schema

This catches SQL typos, missing columns, and type mismatches at compile time — not at runtime.

---

## 6. Recommended Technology Stack (Final)

### Rust Crates
| Category | Crate | Rationale |
|----------|-------|-----------|
| Async runtime | `tokio` | Industry standard, best ecosystem |
| HTTP client | `reqwest` | HTTP/2, connection pooling, built on hyper |
| WebSocket | `tokio-tungstenite` | Battle-tested, thread-safe, good perf |
| Web framework | `axum` | Tokio-native, ergonomic, fast |
| Database | `sqlx` | Compile-time checked queries, async |
| Redis | `fred` | Connection pooling, pipeline batching |
| Messaging | `async-nats` | JetStream, request/reply |
| Serialization | `serde` + `simd-json` | simd-json on hot path |
| Decimal | `rust_decimal` | No floating point in money path |
| Crypto | `openssl` or `aws-lc-rs` | Fastest RSA-SHA256 signing |
| Observability | `tracing` + `tracing-subscriber` | Structured logging, spans |
| Metrics | `metrics` + `metrics-exporter-prometheus` | Optional Prometheus export |
| Concurrency | `dashmap` | Lock-free concurrent maps |
| Config | `dotenvy` + `serde` | Env loading + typed config |
| Embedded KV | `sled` | WAL for crash recovery |

### Python Packages
| Category | Package | Rationale |
|----------|---------|-----------|
| HTTP | `httpx` | Async, HTTP/2, retry built-in |
| WebSocket | `websockets` | Purpose-built, reliable |
| Database | `asyncpg` | Fastest PostgreSQL driver |
| Redis | `redis[hiredis]` | C-backed, streams support |
| Messaging | `nats-py` | NATS client |
| Validation | `pydantic` v2 | Rust-backed validation |
| Math | `numpy` + `scipy` | Stats, normal CDF |
| Data | `polars` | Faster than pandas for analytics |
| Package mgmt | `uv` | Fast, modern |
| Testing | `pytest` + `pytest-asyncio` | Async test support |

### Infrastructure
| Component | Choice | Rationale |
|-----------|--------|-----------|
| Database | TimescaleDB (PG extension) | Time-series optimization |
| Messaging | NATS JetStream | Low-latency, at-least-once |
| Cache/KV | Redis 7 | Orderbook state, rate limiting |
| Containers | Docker Compose | Local development |
| Task runner | `just` | Cross-platform, simple |

---

## 7. Risk Assessment of Current Plan

### What the plan gets right
- **Rust for execution, Python for models** — correct language split
- **Risk manager as a hard gate** — no order bypasses risk
- **Circuit breaker pattern** — prevents runaway losses
- **Quarter-Kelly sizing** — conservative, appropriate for binary options
- **Continuous data collection** — builds backtest depth passively
- **Calibration feedback loop** — the plan's strongest strategic decision

### What needs attention
1. **Single-point-of-failure in weather model** — one σ parameter is too fragile (addressed in 1.3, 4.1)
2. **No exit strategy** — hold-to-settlement leaves money on the table (addressed in 4.3)
3. **No correlation awareness** — concentrated positions in one city (addressed in 4.4)
4. **No idempotency** — crash during order placement risks duplicates (addressed in 3.1)
5. **No alerting** — unattended operation needs notifications (addressed in 3.4)
6. **Plain PostgreSQL for time-series** — will scale poorly (addressed in 1.2)
