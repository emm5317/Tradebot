# CLAUDE.md — Tradebot Project Guide

## What This Project Is

Algorithmic trading bot for Kalshi prediction markets. Trades weather temperature contracts and Bitcoin crypto binary options. Rust handles low-latency execution and exchange feeds; Python handles weather signal generation and fair-value models.

## Architecture Overview

- **Rust binary** (`rust/src/main.rs`): Kalshi WS feeds, crypto exchange feeds (Coinbase, Binance, Deribit), in-process CryptoState, event-driven crypto evaluation, order state machine, execution engine
- **Python evaluator** (`python/evaluator/daemon.py`): Weather-only signal evaluation on 10s cycle, reads METAR/HRRR/ASOS data, publishes signals via NATS
- **Python collector** (`python/collector/daemon.py`): Data collection daemon (ASOS, METAR, HRRR, market snapshots)
- **Python dashboard** (`python/dashboard/app.py`): FastAPI + htmx + SSE on port 8050
- **Grafana** (`docker/grafana/`): Observability dashboards on port 3033, reads from Postgres directly
- **Infrastructure**: PostgreSQL/TimescaleDB, Redis, NATS, Grafana, Docker Compose

## Key Conventions

### Rust
- Entry point: `rust/src/main.rs`
- Config: `rust/src/config.rs` (env vars via `envy`)
- All crypto feeds use persistent WebSocket with exponential backoff reconnect
- CryptoState uses `std::sync::RwLock` (low write rate, ~8/sec)
- OrderbookManager and FeedHealth use `DashMap` (high key cardinality)
- TradeTape shared via `Arc<std::sync::RwLock<TradeTape>>` — extract data before `.await` to avoid Send issues
- Auth uses pure-Rust `rsa` crate (not openssl) for RSA-PSS signing
- Tests are inline `#[cfg(test)] mod tests` in each module

### Python
- Config: `python/config.py` (pydantic Settings)
- Models in `python/models/` — weather_fv.py is the primary active model
- `crypto_fv.py` and `signals/crypto.py` are deprecated (ported to Rust)
- Tests: `python/tests/` with pytest, run via `just test-python`
- Structlog for logging throughout

### Database
- Migrations in `migrations/` (001-018), run via `just migrate` (sqlx-cli)
- All use `IF NOT EXISTS` / `ON CONFLICT` — safe to re-run
- Key tables: `contracts`, `signals`, `orders`, `observations`, `market_snapshots`, `station_calibration`

### Testing
- `just test` — 116 Rust tests
- `just test-python` — 322 Python tests
- `just test-all` — both (438 total)
- Python tests that need DB (asyncpg) will show collection errors locally — this is expected

## Build & Run

```bash
just db-up          # Start Postgres, Redis, NATS via Docker Compose
just migrate        # Run SQL migrations
just dev            # Start Rust binary (all feeds + execution)
just evaluator      # Start Python weather evaluator
just collector      # Start data collection
just dashboard      # Start dashboard on :8050
just grafana        # Start Grafana on :3033
just test           # Run Rust tests (116)
just test-python    # Run Python tests (322)
just test-all       # Run both (438 total)
```

## Important Files

| File | Purpose |
|------|---------|
| `rust/src/crypto_state.rs` | CryptoState with dynamic venue weighting (RTI) |
| `rust/src/crypto_fv.rs` | Crypto fair value: N(d2), Levy, basis, funding |
| `rust/src/crypto_evaluator.rs` | Event-driven crypto eval + microstructure |
| `rust/src/order_manager.rs` | 10-state order lifecycle machine |
| `rust/src/orderbook_feed.rs` | Kalshi WS → OrderbookManager → Redis |
| `python/models/weather_fv.py` | Settlement-aware weather fair value |
| `python/models/physics.py` | Gaussian ensemble + StationCalibration |
| `python/models/rounding.py` | METAR C→F rounding ambiguity + boundary prob |
| `python/evaluator/daemon.py` | Weather evaluation loop |
| `python/rules/resolver.py` | Contract rules resolver |
| `python/backtester/engine.py` | Full historical backtester |
| `python/backtester/sweep.py` | Parameter sweep + walk-forward optimization |
| `python/calibrator/daemon.py` | Calibration agent: hourly feedback loop |
| `python/analytics/settlement_summary.py` | Daily settlement summary aggregation |
| `python/analytics/aggregator.py` | Per-strategy analytics & Brier scoring |
| `python/sync_contracts.py` | Kalshi contract sync (active + settled) |
| `rust/src/clock.rs` | Clock discipline (HTTP Date header sync) |
| `rust/src/dead_letter.rs` | Dead-letter handling (NATS + DB persistence) |
| `rust/src/integration_tests.rs` | Integration test scenarios |
| `rust/src/feed_health.rs` | Per-feed health scoring (0.0-1.0) |
| `rust/src/decision_log.rs` | Decision audit + feed health DB writes |

## Common Pitfalls

- **OpenSSL not needed**: Kalshi auth uses pure-Rust `rsa` crate, not system OpenSSL
- **std::sync::RwLock across .await**: Extract data from lock guards before any `.await` (see TapeSnapshot pattern in `orderbook_feed.rs`)
- **Binance endpoint**: Uses `binance.us` (not `.com`) for US compliance
- **Weather model backward compat**: New params (station_cal, metar_temp_c, hrrr_forecast_temps_f) all default to None — existing callers work unchanged
- **Test count**: Some Python test files (test_kalshi_history, test_rules) need asyncpg and may show collection errors in local dev — ignore these

## Implementation Phases

Phases 0-9.0 are complete. Phase 5 added: per-strategy analytics & Brier scoring (5.1), calibration dashboard (5.2), P&L attribution with model_components JSONB (5.3), reconciliation loop (5.4), clock discipline (5.5), dead-letter handling (5.6), integration tests (5.7), per-feed health scoring (5.8). Phase 6.1: parameter sweep framework, daily settlement summary, collector enhancements (crypto_ticks, settlement aggregation). Phase 7: calibration agent & prediction feedback loop — fixes signal_id/outcome/latency_ms data plumbing, adds calibration daemon with 6 hourly jobs, confidence-scaled order sizing, book-walking fill estimation, VWAP microstructure signal, evaluator hot-reload, price momentum (7.3a), volume surge detection (7.3c), OI delta tracking (7.3d), edge trajectory tracking (7.5). Phase 8: advanced backtesting & adaptive calibration — foundation fixes (8.0a-e), transaction costs (8.2), advanced metrics (8.3), crypto threshold sweep (8.1), multi-signal eval (8.4), parallel sweep (8.5), replay engine with source ablation (8.6), comprehensive tests (8.7). Phase 9.0: Grafana observability — decision_log + feed_health_log tables, 4 auto-provisioned dashboards, 5 alert rules (Discord), Rust + Python decision trace instrumentation. See `docs/build-plans/phase-8-backtesting.md`.
