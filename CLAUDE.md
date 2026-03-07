# CLAUDE.md
Context file for AI-assisted development on `tradebot`.
## Project Overview
Automated near-expiry settlement trading system for Kalshi event contracts. Two asset classes: weather (ASOS observation arbitrage) and crypto (Black-Scholes binary option pricing). Python generates signals, Rust executes orders. Redis Streams bridges them. PostgreSQL 17 is the single source of truth.
This is a solo developer project. The developer is an experienced litigator and software engineer building this as a personal trading system.
## Repo Layout
```
c:\dev\tradebot\
├── rust\           Execution engine — Cargo project, Rust 2021 edition
│   └── src\
│       ├── kalshi\     API client (REST + WebSocket), RSA auth
│       ├── execution\  Order placement, position tracking, Kelly sizing
│       ├── risk\       RiskManager, circuit breaker, kill switch
│       ├── signal\     Redis Streams consumer
│       ├── ui\         Axum routes serving terminal UI
│       └── db\         sqlx queries (compile-time checked)
├── python\         Signal generation + backtesting
│   ├── signals\    Settlement scanner, weather/crypto evaluators
│   ├── data\       ASOS, Binance WS, Kalshi history, Mesonet fetchers
│   ├── models\     Physics probability model, Black-Scholes binary option
│   ├── backtest\   Backtest runners and analysis
│   └── utils\      Kelly criterion, macro event blackout
├── migrations\     Flat SQL files, managed by sqlx-cli (001_, 002_, etc.)
├── ui\             terminal.html — single-page terminal UI
├── config\         .env.example, stations.json, blackout_events.json
├── docker\         docker-compose.yml, Dockerfiles
└── scripts\        PowerShell (.ps1) automation scripts
```
## Technology Decisions — Do Not Deviate
- **Rust** for execution engine. All order placement, risk checks, and position management happen in Rust.
- **Python** for signal generation, backtesting, and data fetching. Models iterate faster in Python.
- **Redis Streams** for Python→Rust signal bridge. Not gRPC, not HTTP, not message queues.
- **PostgreSQL 17** only. No SQLite, no other databases.
- **sqlx** for Rust DB access with compile-time checked queries. No ORMs.
- **Axum** for the UI backend. No other web frameworks.
- **just** as task runner (not make, not npm scripts).
- **rust_decimal** for all money/price values. Never f64 for prices.
- **simd-json** on the hot path. serde_json is fine for config/logging.
- **Atomic operations** (AtomicI64, AtomicBool) for balance/exposure/kill switch on the hot path. No mutex on the order placement critical path.
- **DashMap** for concurrent position storage.
- **crossbeam-channel** for signal→order pipeline.
## Coding Conventions
### Rust
- 2021 edition
- All money values stored as integer cents (`i64`) or `rust_decimal::Decimal`
- Error handling: `anyhow::Result` for application code, custom errors only if a crate boundary demands it
- Logging: `tracing` crate with structured fields. Every order and risk decision gets a span.
- Async everywhere via tokio. No blocking calls on the execution path.
- Tests: `#[tokio::test]` for async tests. Risk manager must have 100% coverage of limit enforcement.
### Python
- Python 3.11+
- Type hints on all function signatures
- `httpx` for HTTP (async where possible)
- `numpy` + `scipy.stats.norm` for math
- Signals published to Redis as JSON with schema: `{ticker, direction, edge, kelly, confidence, model_prob, market_price, timestamp}`
### SQL
- All times in UTC (`TIMESTAMPTZ`)
- Money stored as integer cents (`INTEGER`, column name ends in `_cents`)
- Migrations are sequential flat files: `001_contracts.sql`, `002_signals.sql`, etc.
- Run via `sqlx-cli`: `sqlx migrate run`
### General
- Config via `.env` files loaded by `dotenvy`
- No hardcoded API keys, URLs, or credentials anywhere in source
- Windows paths throughout (project lives at `c:\dev\tradebot\`)
## Risk Framework — Non-Negotiable Invariants
These are hard limits enforced in `rust/src/risk/manager.rs`. They cannot be relaxed without explicit developer approval:
| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max loss per trade | $25 (2500 cents) | `RiskManager::approve_order()` |
| Max daily loss | $100 (10000 cents) | `AtomicI64` checked pre-order |
| Max open positions | 4 | `DashMap::len()` check |
| Max total exposure | $60 (6000 cents) | Sum of open position sizes |
| Min time to settlement | 2.5 minutes | Time gate in `process_signal()` |
| Max time to settlement | 18 minutes | Time gate in `process_signal()` |
| Kill switch | AtomicBool | Checked first on every signal |
| Circuit breaker | 3 losses in 30 min | Auto-pause 1 hour |
**When writing or modifying risk code**: every limit must have a corresponding unit test that proves the limit blocks an order that would violate it. No exceptions.
## Key External APIs
| API | Purpose | Auth | Docs |
|-----|---------|------|------|
| Kalshi Trading API v2 | Orders, markets, positions | RSA-SHA256 signed requests | https://trading-api.kalshi.com/trade-api/v2 |
| Kalshi WebSocket v2 | Real-time orderbook | Same auth, WS upgrade | wss://trading-api.kalshi.com/trade-api/ws/v2 |
| Binance WebSocket | BTC spot price stream | None (public) | wss://stream.binance.com:9443 |
| Iowa State Mesonet | 1-minute ASOS observations | None (public) | https://mesonet.agron.iastate.edu/request/asos/1min.php |
| Aviation Weather | METAR reports | None (public) | https://aviationweather.gov/api/data/metar |
## Signal Schema
Signals flow from Python → Redis Streams → Rust. The canonical schema:
```json
{
  "ticker": "KXTEMP-24-HI-T68-20240715",
  "direction": "yes",
  "edge": 0.08,
  "kelly_fraction": 0.062,
  "model_prob": 0.85,
  "market_price": 0.77,
  "category": "weather",
  "minutes_to_settlement": 12.5,
  "timestamp": "2024-07-15T19:48:00Z"
}
```
Both sides must agree on this schema. If you change a field, update both the Python publisher (`python/signals/publisher.py`) and the Rust consumer (`rust/src/signal/consumer.rs`).
## Common Tasks
```powershell
just build              # cargo build --release in rust/
just test               # cargo test + pytest
just migrate            # sqlx migrate run
just db-up              # docker compose up postgres redis
just db-down            # docker compose down
just backtest-weather   # python -m backtest.weather_backtest
just backtest-crypto    # python -m backtest.crypto_backtest
just paper              # start both engines in paper mode
just lint               # cargo clippy + ruff
```
## What Not To Do
- Do not place live orders without backtest validation. Paper mode exists for a reason.
- Do not use f64 for money. Ever. Use `rust_decimal::Decimal` or integer cents.
- Do not add a mutex to the order hot path. Use atomics and lock-free structures.
- Do not poll Kalshi REST for market prices. Use the WebSocket feed.
- Do not use NWS hourly observations for near-expiry weather. Use Iowa Mesonet 1-minute data.
- Do not enter trades within 2.5 minutes of settlement. Fills may not process.
- Do not suppress or bypass the circuit breaker. It exists for a reason.
- Do not commit `.env` files or credentials to the repo.
