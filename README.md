# Tradebot

Automated binary options trading system for Kalshi, targeting weather and crypto markets. Dual-language architecture: Python signal engine + Rust execution layer, connected via NATS messaging.

> **Build Status**: All 10 planned improvement phases are implemented. See [docs/BUILD_STATUS.md](docs/BUILD_STATUS.md) for detailed completion status and known issues.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                   PYTHON SIGNAL ENGINE                        │
├──────────────────────────────────────────────────────────────┤
│  CollectorDaemon (always-on, 60s cycles)                     │
│  ├─ ASOS weather observations (10 stations, concurrent)      │
│  ├─ Kalshi market snapshots (near-settlement contracts)      │
│  └─ Binance BTC spot + 30-min realized volatility            │
│                                                              │
│  EvaluationDaemon (10s cycles)                               │
│  ├─ WeatherSignalEvaluator (Gaussian + climo + trend)        │
│  ├─ CryptoSignalEvaluator  (Black-Scholes + realized vol)    │
│  └─ Evaluator plugin registry (extensible for new markets)   │
│                                                              │
│  SignalPublisher (dual-write)                                │
│  ├─ NATS → tradebot.signals (triggers execution)            │
│  ├─ DB   → signals table (audit trail, async)               │
│  └─ Redis → model_state:{ticker} (dashboard display)        │
│                                                              │
│  Backtester (offline)                                        │
│  └─ Replays historical data through evaluators               │
│                                                              │
│  Dashboard (FastAPI + htmx, terminal-style UI)               │
│  └─ SSE from Redis/NATS → live signals, model state, P&L    │
├──────────────────────────────────────────────────────────────┤
│                   RUST EXECUTION ENGINE                       │
├──────────────────────────────────────────────────────────────┤
│  NATS Consumer → deserialize SignalSchema                    │
│  ├─ Position manager (prevents double-entry)                 │
│  ├─ Risk checks (max size, daily loss, exposure limits)      │
│  ├─ KalshiClient.place_order() (RSA-PSS signed)             │
│  └─ Order tracking → orders table                            │
│                                                              │
│  WebSocket Orderbook (real-time bid/ask/depth)               │
│  └─ DashMap-based in-memory orderbook per ticker             │
│                                                              │
│  Notifier (Discord webhooks)                                 │
│  └─ Signal alerts, fill notifications, error reports         │
└──────────────────────────────────────────────────────────────┘
```

## Project Structure

```
Tradebot/
├── config/                   # Environment configuration
│   └── .env.example          # All environment variables
├── docker/                   # Docker Compose (Postgres, Redis, NATS)
├── migrations/               # SQL migrations (TimescaleDB)
│   ├── 001_contracts.sql
│   ├── 002_signals.sql
│   ├── 003_orders.sql
│   ├── 004_daily_summary.sql
│   ├── 005_observations.sql
│   ├── 006_market_snapshots.sql
│   ├── 007_calibration.sql
│   └── 008_blackout_events.sql
├── python/
│   ├── collector/            # Data collection daemon
│   │   └── daemon.py
│   ├── data/                 # Data source clients
│   │   ├── binance_ws.py     # BTC WebSocket feed + OHLC bars
│   │   ├── mesonet.py        # ASOS weather observations
│   │   └── kalshi_history.py # Historical market data
│   ├── models/               # Pricing models
│   │   ├── physics.py        # Gaussian diffusion + ensemble
│   │   └── binary_option.py  # Black-Scholes for binary options
│   ├── signals/              # Signal evaluation engine
│   │   ├── types.py          # Shared schemas (Pydantic)
│   │   ├── utils.py          # Shared fill estimation + Kelly sizing
│   │   ├── registry.py       # Evaluator plugin registry
│   │   ├── weather.py        # Weather signal evaluator
│   │   ├── crypto.py         # Crypto signal evaluator
│   │   ├── publisher.py      # NATS + DB dual-write publisher
│   │   └── notifier.py       # Discord webhook notifications
│   ├── evaluator/            # Signal orchestration
│   │   └── daemon.py         # Evaluation loop (10s cycle)
│   ├── backtester/           # Historical replay framework
│   │   └── engine.py         # Backtesting engine
│   ├── dashboard/            # Terminal-style web UI
│   │   ├── app.py            # FastAPI + SSE endpoints
│   │   ├── static/           # CSS + minimal JS
│   │   └── templates/        # Jinja2 HTML templates
│   ├── config.py             # Pydantic settings
│   ├── pyproject.toml
│   └── tests/
├── rust/
│   ├── src/
│   │   ├── main.rs            # Entry point, service orchestration, graceful shutdown
│   │   ├── config.rs          # Configuration from env vars (envy)
│   │   ├── logging.rs         # Structured logging (tracing, JSON/pretty)
│   │   ├── execution.rs       # NATS signal consumer, risk checks, order execution
│   │   ├── orderbook_feed.rs  # WebSocket → in-memory orderbook → Redis bridge
│   │   └── kalshi/            # Kalshi exchange integration
│   │       ├── auth.rs        # RSA-PSS request signing
│   │       ├── client.rs      # REST API client (retry, error parsing)
│   │       ├── websocket.rs   # Persistent WS feed (auto-reconnect)
│   │       ├── orderbook.rs   # In-memory orderbook (BTreeMap, DashMap)
│   │       ├── types.rs       # API types (Market, Order, Position)
│   │       └── error.rs       # Error handling (KalshiError enum)
│   ├── Cargo.toml
│   └── Cargo.lock
└── justfile                   # Task runner
```

## Quick Start

```bash
# 1. Infrastructure
just db-up                     # Start Postgres, Redis, NATS

# 2. Configure & migrate
cp config/.env.example .env    # Configure credentials
just migrate                   # Run SQL migrations

# 3. Collect data
just collector                 # Start data collection daemon

# 4. Run evaluator (after collector has data)
just evaluator                 # Signal evaluation loop (10s cycle)

# 5. Start execution engine
just dev                       # Rust NATS consumer + order execution

# 6. Dashboard
just dashboard                 # Terminal-style UI on :8050

# 7. Run tests
just test-all                  # Rust + Python tests

# 8. Backtest (optional)
just backtest 2024-01-01 2024-06-30   # Historical replay
```

## Trading Models

### Weather (Ensemble)
Three-model ensemble for temperature binary contracts:
- **Physics**: Gaussian diffusion (P(T >= threshold) using erfc-based CDF)
- **Climatology**: Historical (station, hour, month) distribution blended with current obs
- **Trend**: Least-squares linear extrapolation from recent readings

Default weights: 50% physics, 25% climo, 25% trend. Station-specific sigma from DB.

### Crypto (Black-Scholes)
Single-model approach for BTC binary contracts:
- Risk-neutral d2 probability from Black-Scholes
- 30-min realized volatility from 1-min OHLC bars (EWMA-weighted)
- Blackout windows for scheduled events (FOMC, etc.)

### Shared Signal Logic
- Spread-adjusted edge (raw edge - half spread, 15% discount if spread > 10%)
- Kelly criterion using estimated fill price (best ask for YES, best bid for NO)
- Signal cooldown (300s per ticker)
- Exit signals when accumulated edge flips below -3%

## Configuration

Key environment variables (see `config/.env.example` for full list):

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL connection | `postgres://tradebot:...` |
| `REDIS_URL` | Redis for model state | `redis://localhost:6379` |
| `NATS_URL` | NATS messaging | `nats://localhost:4222` |
| `PAPER_MODE` | Paper trading mode | `true` |
| `MAX_TRADE_SIZE_CENTS` | Per-order limit | `2500` ($25) |
| `MAX_DAILY_LOSS_CENTS` | Daily stop-loss | `10000` ($100) |
| `MAX_POSITIONS` | Max concurrent positions | `5` |
| `KELLY_FRACTION_MULTIPLIER` | Kelly scaling factor | `0.25` |
| `DISCORD_WEBHOOK_URL` | Alert notifications | (optional) |

## Development

```bash
just test-python     # Python tests (pytest, 11 test files)
just test            # Rust tests (cargo test)
just test-all        # Both
just fmt             # Format Rust code
just clippy          # Rust lints
just health          # Check system health (requires running dashboard)
```

## Risk Controls

The system enforces multiple safety layers:

| Control | Default | Description |
|---------|---------|-------------|
| Paper mode | `true` | Logs orders without executing — must explicitly disable |
| Max trade size | $25 | Per-order size cap |
| Daily loss limit | $100 | Circuit breaker stops all trading |
| Max positions | 5 | Concurrent position limit |
| Max exposure | $150 | Total capital at risk |
| Kelly multiplier | 0.25 | Quarter-Kelly for conservative sizing |
| Signal cooldown | 300s | Prevents signal flooding per ticker |
| Idempotency keys | — | Prevents duplicate fills on NATS redelivery |
| Blackout windows | — | Pauses crypto trading during FOMC/scheduled events |
