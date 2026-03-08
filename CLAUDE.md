# CLAUDE.md — Tradebot Project Guide

## What This Project Is

Algorithmic trading bot for Kalshi prediction markets. Trades weather temperature contracts and Bitcoin crypto binary options. Rust handles low-latency execution and exchange feeds; Python handles weather signal generation and fair-value models.

## Architecture Overview

- **Rust binary** (`rust/src/main.rs`): Kalshi WS feeds, crypto exchange feeds (Coinbase, Binance, Deribit), in-process CryptoState, event-driven crypto evaluation, order state machine, execution engine
- **Python evaluator** (`python/evaluator/daemon.py`): Weather-only signal evaluation on 10s cycle, reads METAR/HRRR/ASOS data, publishes signals via NATS
- **Python collector** (`python/collector/daemon.py`): Data collection daemon (ASOS, METAR, HRRR, market snapshots)
- **Python dashboard** (`python/dashboard/app.py`): FastAPI + htmx + SSE on port 8050
- **Infrastructure**: PostgreSQL/TimescaleDB, Redis, NATS, Docker Compose

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
- Migrations in `migrations/` (001-015), run via `just migrate` (sqlx-cli)
- All use `IF NOT EXISTS` / `ON CONFLICT` — safe to re-run
- Key tables: `contracts`, `signals`, `orders`, `observations`, `market_snapshots`, `station_calibration`

### Testing
- `just test` — 92 Rust tests
- `just test-python` — 236 Python tests
- `just test-all` — both (328 total)
- Python tests that need DB (asyncpg) will show collection errors locally — this is expected

## Build & Run

```bash
just db-up          # Start Postgres, Redis, NATS via Docker Compose
just migrate        # Run SQL migrations
just dev            # Start Rust binary (all feeds + execution)
just evaluator      # Start Python weather evaluator
just collector      # Start data collection
just dashboard      # Start dashboard on :8050
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

## Common Pitfalls

- **OpenSSL not needed**: Kalshi auth uses pure-Rust `rsa` crate, not system OpenSSL
- **std::sync::RwLock across .await**: Extract data from lock guards before any `.await` (see TapeSnapshot pattern in `orderbook_feed.rs`)
- **Binance endpoint**: Uses `binance.us` (not `.com`) for US compliance
- **Weather model backward compat**: New params (station_cal, metar_temp_c, hrrr_forecast_temps_f) all default to None — existing callers work unchanged
- **Test count**: Some Python test files (test_kalshi_history, test_rules) need asyncpg and may show collection errors in local dev — ignore these

## Implementation Phases

Phases 0-5.8 are complete. Phase 5 added: per-strategy analytics & Brier scoring (5.1), calibration dashboard (5.2), P&L attribution with model_components JSONB (5.3), reconciliation loop (5.4), clock discipline (5.5), dead-letter handling (5.6), integration tests (5.7), per-feed health scoring (5.8). See `docs/build-plans/` for detailed specs.
