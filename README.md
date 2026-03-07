# Tradebot

Automated trading system for Kalshi prediction markets. Combines a Rust execution engine with Python-based signal generation, backtesting, and data ingestion.

## Architecture

```
Python (signals, data, models, backtest)
        │
        ▼  Redis pub/sub
Rust (execution, risk, Kalshi API, UI)
        │
        ▼  PostgreSQL
   Persistence & analytics
```

### Rust Engine (`rust/`)
- **kalshi/** – REST + WebSocket client for the Kalshi API
- **execution/** – Order management, position tracking, sizing
- **risk/** – Risk manager, circuit breaker, kill switch
- **signal/** – Redis consumer for Python-generated signals
- **ui/** – Axum web server serving `terminal.html`
- **db/** – PostgreSQL queries via sqlx

### Python Signals (`python/`)
- **signals/** – Market scanner, weather and crypto signal generators
- **data/** – ASOS, Binance WebSocket, Kalshi history, Mesonet clients
- **models/** – Physics-based weather models, binary option pricing
- **backtest/** – Strategy backtesting framework
- **utils/** – Kelly criterion sizing, blackout period enforcement

## Quick Start

1. Copy `config/.env.example` to `config/.env` and fill in credentials
2. Start infrastructure: `docker compose -f docker/docker-compose.yml up -d postgres redis`
3. Run migrations: `.\scripts\setup_db.ps1`
4. Start the engine: `cd rust && cargo run`
5. Start signals: `cd python && python -m signals.scanner`

## Paper Trading

```powershell
.\scripts\paper_trade.ps1
```

## Backtesting

```powershell
.\scripts\run_backtest.ps1 weather
.\scripts\run_backtest.ps1 crypto
```
