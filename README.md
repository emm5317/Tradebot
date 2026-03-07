# tradebot
Near-expiry settlement trading on [Kalshi](https://kalshi.com) — weather and crypto contracts.
Rust execution engine + Python signal layer. Not prediction. Arbitrage: reading present reality faster than the market does.
---
## What This Does
Kalshi event contracts settle at known times. In the final 8–18 minutes before settlement, the outcome is largely determined by observable data (current temperature, current BTC price) but the market often lags by 1–3 minutes. This system exploits that lag.
**Weather contracts**: Compares real-time ASOS weather station observations against contract thresholds using a physics-based probability model. When the market's implied probability diverges from the observation-derived probability by >5%, it trades.
**Crypto contracts**: Computes Black-Scholes binary option probability from BTC spot price, distance to threshold, and 30-minute realized volatility. When the market diverges from N(d2) by >6 cents, it trades.
## Architecture
```
Python (signals)                    Rust (execution)
┌──────────────────┐               ┌──────────────────────┐
│ ASOS observations│               │ Redis consumer       │
│ Binance WS feed  │──► Redis ──►  │ Risk check (atomics) │
│ Physics / BS model│  Streams     │ Kelly sizing         │
│ Settlement scanner│               │ Kalshi HTTP/2 order  │
└──────────────────┘               └──────────────────────┘
                                            │
                        ┌───────────────────┤
                        ▼                   ▼
                   PostgreSQL 17       Axum UI server
```
Target latency: <50ms from signal to order acknowledgment.
## Tech Stack
| Layer | Tech | Why |
|-------|------|-----|
| Execution engine | Rust (tokio, reqwest, axum) | Lock-free hot path, sub-50ms orders |
| Signal generation | Python (numpy, scipy, httpx) | Fast iteration on models |
| Bridge | Redis Streams | <1ms local pub/sub |
| Database | PostgreSQL 17 | Single source of truth |
| Market data | Kalshi WebSocket, Binance WebSocket | ~10ms price updates |
| Weather data | Iowa State Mesonet (1-min ASOS) | Freshest available observations |
| Task runner | just | Native Windows, no shell overhead |
## Project Structure
```
tradebot/
├── rust/              Execution engine (Cargo workspace)
├── python/            Signal generation + backtesting
├── migrations/        PostgreSQL schema (sqlx-cli)
├── ui/                Terminal UI (served by Axum)
├── config/            Environment, stations, blackout events
├── docker/            Compose + Dockerfiles
├── scripts/           PowerShell automation
├── justfile           Task runner
├── CLAUDE.md          AI assistant context
├── soul.md            Project philosophy
├── ARCHITECTURE.md    System design reference
├── RISK.md            Risk framework (hard limits)
└── DATA_SOURCES.md    External data source reference
```
## Quick Start
### Prerequisites
- Rust (via rustup)
- Python 3.11+
- PostgreSQL 17
- Redis
- Docker (optional, for containerized Postgres/Redis)
### Setup
```powershell
# Clone
git clone <repo-url>
cd tradebot
# Infrastructure
just db-up              # Start Postgres + Redis via Docker
just migrate            # Run SQL migrations
# Rust execution engine
just build              # cargo build --release
# Python signal layer
just venv               # Create venv + install deps
just backtest-weather   # Run weather backtest first
```
### Paper Trading
```powershell
just paper              # Starts both signal engine + execution engine in paper mode
```
**Do not run live until backtests confirm positive edge and RiskManager is battle-tested in paper mode.**
## Risk Limits
| Parameter | Value |
|-----------|-------|
| Starting bankroll | $500 |
| Max loss per trade | $25 |
| Max daily loss | $100 |
| Max open positions | 4 |
| Max total exposure | $60 |
| Settlement time window | 2.5–18 min |
| Circuit breaker | 3 losses in 30 min → 1hr pause |
| Kill switch | Instant, via UI or API |
These are enforced in Rust with atomic operations. They are not suggestions. See [RISK.md](RISK.md).
## Edge Hypotheses
Both hypotheses are designed to be falsifiable via backtest before any capital is deployed.
1. **Weather observation arbitrage** — ASOS station data confirms/denies contract thresholds before the market prices it in. Physics model assigns probability; trade when divergence >5%.
2. **Crypto proximity arbitrage** — Black-Scholes binary option pricing from realized 30-min vol vs. market's stale implied vol. Trade when divergence >6 cents.
## Phase Plan
| Phase | Weeks | Focus |
|-------|-------|-------|
| 1 | 1–3 | Data plumbing: Kalshi client, ASOS fetcher, Binance WS, DB schema |
| 2 | 3–5 | Backtesting: validate edge hypotheses with historical data |
| 3 | 4–6 | Signal engine: scanner, physics model, Redis publishing |
| 4 | 5–9 | Rust execution: risk manager, order placement, position tracking |
Phase gates are hard. No live trading without backtest confirmation.
## License
Private. Not open source.
