# Build Status & Code Review

Last reviewed: 2026-03-08

## Overall Status: ~85% Feature-Complete

All 10 planned improvement phases are implemented. The system is functional end-to-end in paper mode. Several code quality issues and integration gaps remain before production readiness.

---

## Component Status

### Python Signal Engine

| Component | Status | Notes |
|-----------|--------|-------|
| Collector daemon | **Working** | ASOS (10 stations), Kalshi snapshots (concurrent), BTC feed |
| Evaluator daemon | **Working** | 10s cycle, registry dispatch, NATS + DB + Redis publish |
| Weather evaluator | **Working** | Gaussian ensemble (physics + climo + trend), entry/exit signals |
| Crypto evaluator | **Working** | Black-Scholes + EWMA vol, blackout windows |
| Signal publisher | **Working** | Dual-write NATS + DB, Redis model state |
| Discord notifier | **Working** | Signal alerts, error reporting, daily summaries |
| Dashboard | **Working** | FastAPI + SSE + htmx, terminal aesthetic on :8050 |
| Backtester | **Working** | Historical replay, accuracy/Brier/calibration/P&L metrics |
| Evaluator registry | **Working** | Pluggable evaluator pattern, weather + crypto registered |
| Shared utils | **Working** | Fill price estimation, Kelly sizing, edge computation |
| Tests | **11 files, ~1140 LOC** | Physics, evaluators, publisher, registry, utils, data feeds |

### Rust Execution Engine

| Component | Status | Notes |
|-----------|--------|-------|
| Service bootstrap (main.rs) | **Working** | PostgreSQL, Redis, NATS, Kalshi auth, graceful shutdown |
| Configuration (config.rs) | **Working** | Env var parsing via envy, secret redaction |
| Execution engine (execution.rs) | **Working** | NATS consumer, risk checks, order placement, DB recording |
| Orderbook feed (orderbook_feed.rs) | **Working** | WS → in-memory orderbook → Redis bridge (500ms flush) |
| Kalshi auth (auth.rs) | **Working** | RSA-PSS signing with SHA-256 |
| Kalshi REST client (client.rs) | **Working** | GET/POST/DELETE with 3-attempt retry |
| Kalshi WebSocket (websocket.rs) | **Working** | Persistent connection, auto-reconnect, ping/pong |
| Orderbook (orderbook.rs) | **Working** | BTreeMap levels, snapshot/delta, mid-price/spread/fill estimation |
| Error handling (error.rs) | **Working** | Comprehensive KalshiError enum |
| API types (types.rs) | **Working** | Serde types for all Kalshi API objects |

### Infrastructure

| Component | Status | Notes |
|-----------|--------|-------|
| Docker Compose | **Ready** | TimescaleDB 17, Redis 7, NATS 2 with JetStream |
| Migrations | **8 files** | contracts, signals, orders, daily_summary, observations, market_snapshots, calibration, blackout_events |
| Justfile | **Complete** | All build/test/run commands |
| .env.example | **Complete** | All configuration variables documented |

---

## Known Issues

### Rust — Code Quality

| Severity | File | Issue |
|----------|------|-------|
| Medium | execution.rs:329 | `tracker.positions` accessed directly (private field); works because it's in the same module but fragile |
| Medium | execution.rs:388,391 | `i64` to `i32` casts for `size_cents` and `latency_ms` could overflow |
| Medium | execution.rs:328 | Misleading comment "Sell the opposite side" — code correctly sells the same side held |
| Low | execution.rs:355 | P&L estimation uses `signal.market_price` instead of actual fill price from entry |
| Low | client.rs:38,42,46 | `.unwrap()` on `HeaderValue::from_str()` — could panic on invalid auth header characters |
| Low | config.rs | No validation that risk parameters are positive |
| Low | orderbook_feed.rs:86-89 | Decimal → String → f64 conversion; should use `to_f64()` directly |

### Python — Code Quality

| Severity | File | Issue |
|----------|------|-------|
| Medium | weather.py:73, crypto.py:77 | `_recent_signals` dict grows unbounded (memory leak over time) |
| Medium | evaluator/daemon.py:70 | Blackout windows loaded once at startup, never refreshed |
| Medium | publisher.py:73,99 | Fire-and-forget async tasks; exceptions logged but not propagated |
| Low | dashboard/app.py | No authentication — anyone on the network can view signals |
| Low | evaluator/daemon.py:267 | Inline `import json` inside async method (should be module-level) |
| Low | notifier.py:129 | Notifications lost if rate-limit retry fails; no re-queue |
| Low | binance_ws.py | EWMA variance stays 0 if initialized with fewer than 10 bars |

### Integration Gaps

| Gap | Impact | Description |
|-----|--------|-------------|
| Position persistence | Medium | Rust `PositionTracker` is in-memory only; positions lost on restart |
| Settlement listener | Medium | No task to consume Kalshi settlement events; actual P&L not computed |
| Signal cooldown persistence | Low | Cooldown state is in-memory; duplicates possible across restarts |
| NATS authentication | Low | No auth configured; any network client could publish signals |
| Daily summary trigger | Low | `notify_daily_summary()` implemented but never called on schedule |

---

## Test Coverage

### Python (pytest)

| Test File | Coverage Area |
|-----------|--------------|
| test_physics.py | Gaussian ensemble, CDF, trend extrapolation |
| test_weather_evaluator.py | Weather signal entry/exit, rejection reasons |
| test_crypto_evaluator.py | Crypto signals, blackout windows, vol thresholds |
| test_publisher.py | NATS + DB dual-write, Redis model state |
| test_registry.py | Evaluator registration, lookup, protocol compliance |
| test_utils.py | Kelly sizing, fill estimation, edge computation |
| test_binance.py | BTC feed, OHLC bars, vol calculation |
| test_mesonet.py | ASOS fetch, station parsing, staleness |
| test_kalshi_history.py | Historical data client |
| test_binary_option.py | Black-Scholes pricing, Greeks |

### Rust (cargo test)

| Module | Tests |
|--------|-------|
| kalshi/orderbook.rs | Snapshot/delta application, fill price, imbalance, staleness (5 tests) |

---

## Architecture Strengths

1. **Dual-language optimization**: Python for flexible signal logic, Rust for low-latency execution
2. **NATS decoupling**: Signal engine and execution engine are independent; either can restart without affecting the other
3. **Evaluator registry**: New market types (sports, politics) can be added without touching core logic
4. **Shared utilities**: No code duplication between evaluators for Kelly/fill/edge
5. **Paper mode by default**: Cannot accidentally trade live without explicit opt-in
6. **Idempotency keys**: Prevents duplicate fills on NATS redelivery or network retries
7. **Redis bridge**: Real-time orderbook data from Rust WS feed available to Python evaluator
8. **Structured logging**: Both Python (structlog) and Rust (tracing) emit structured JSON logs

---

## Recommended Next Steps (Priority Order)

### Pre-Production (Required)

1. **Add position persistence** — Load open positions from `orders` table on Rust startup
2. **Implement settlement listener** — Async task to poll/consume Kalshi settlements, compute actual P&L
3. **Add NATS authentication** — Prevent unauthorized signal injection
4. **Add dashboard authentication** — Basic API key or session auth
5. **Fix cooldown dict cleanup** — Add TTL-based expiry or periodic cleanup in evaluators

### Quality Improvements (Recommended)

6. **Validate config parameters** — Ensure risk limits are positive, Kelly multiplier in (0,1]
7. **Replace string enums** — Use typed Rust enums for order side/action/status in types.rs
8. **Fix P&L calculation** — Use actual fill price from entry (stored in PositionTracker) for exit P&L
9. **Add Redis/NATS auth to config** — Password fields for production deployments
10. **Schedule daily summary** — Wire a cron-style trigger for `notify_daily_summary()`

### Future Enhancements

11. **Additional evaluator types** — Sports, politics, or other binary contract markets
12. **Advanced risk controls** — Volatility-adjusted sizing, correlation limits
13. **P&L analytics** — Historical charts, drawdown analysis, Sharpe ratio
14. **Multi-instance support** — Distributed position tracking via Redis
15. **CI/CD pipeline** — Automated testing, linting, and deployment
