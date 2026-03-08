# Tradebot — Automated Kalshi Prediction Market Trading Bot

Algorithmic trading system for [Kalshi](https://kalshi.com) prediction markets. Trades **weather temperature contracts** and **Bitcoin crypto binary options** using settlement-aware fair-value models, real-time WebSocket feeds, and automated order execution.

Built with **Rust** (low-latency execution, exchange feeds) and **Python** (signal generation, fair-value models), connected via NATS messaging and Redis state cache.

> **223 tests** | Rust + Python | Paper trading mode for safe development

---

## Key Features

- **Settlement-aware pricing models** — Models map directly to how Kalshi contracts settle: CFB Real-Time Index for crypto, NWS Daily Climate Reports for weather
- **Multi-exchange crypto feeds** — Coinbase, Binance Futures, Deribit DVOL via persistent WebSocket connections in Rust
- **Weather fair-value engine** — Running max/min tracking, METAR 6-hourly group parsing, HRRR forecast blending, C-to-F rounding ambiguity detection
- **Structured contract rules** — Ticker parser extracts strike, station, settlement source from structured ticker format (no regex-on-title)
- **Real-time orderbook** — DashMap-based in-memory orderbook per ticker with trade tape, aggressiveness metrics, and stale feed detection
- **Risk management** — Kelly criterion sizing, max position limits, daily loss stops, spread-adjusted edge thresholds
- **Historical replay** — Source attribution via Brier score ablation testing to prove each data source adds marginal value
- **Live dashboard** — FastAPI + htmx terminal-style UI with SSE for real-time model state, signals, and P&L

## Architecture

```
                          ┌─────────────────────────────────────────────────┐
                          │              RUST (Low-latency)                 │
                          │                                                 │
  Kalshi WS ──────────────┤  Orderbook Feed                                │
  (orderbook_delta,       │  ├─ DashMap orderbooks (mid/spread/depth)      │
   trade, ticker)         │  ├─ TradeTape (aggressiveness, VWAP, volume)   │
                          │  └─ Redis flush (500ms) ──────────┐            │
                          │                                    │            │
  Coinbase WS ────────────┤  CoinbaseFeed (BTC-USD level2)    │            │
  Binance Futures WS ─────┤  BinanceFuturesFeed (perp/mark)   ├─► Redis   │
  Deribit WS ─────────────┤  DeribitFeed (DVOL index)         │            │
                          │                                    │            │
  NATS signals ───────────┤  Execution Engine                  │            │
                          │  ├─ Position manager               │            │
                          │  ├─ Risk checks                    │            │
                          │  └─ KalshiClient (RSA-PSS signed)  │            │
                          └─────────────────────────────────────────────────┘
                                                               │
                                                               ▼
                          ┌─────────────────────────────────────────────────┐
                          │              PYTHON (Signal Engine)             │
                          │                                                 │
  Redis (orderbooks,      │  EvaluationDaemon (10s cycles)                 │
   crypto feeds) ─────────┤  ├─ ContractRulesResolver (settlement mapping) │
                          │  ├─ WeatherFairValue engine                    │
  ASOS/METAR/HRRR ────────┤  │  ├─ Running max/min lock detection         │
                          │  │  ├─ HRRR forecast excursion blending        │
  Binance spot ───────────┤  │  └─ C→F rounding ambiguity model           │
                          │  ├─ CryptoFairValue engine                     │
                          │  │  ├─ Shadow RTI estimation (Coinbase+Binance)│
                          │  │  ├─ Basis / funding rate signals            │
                          │  │  └─ Deribit DVOL implied volatility         │
                          │  └─ Kelly sizing + edge filters                │
                          │                                                 │
                          │  SignalPublisher ─── NATS ──► Rust execution   │
                          │                  ├── DB (audit trail)          │
                          │                  └── Redis (dashboard state)   │
                          │                                                 │
                          │  ReplayEngine (source attribution)             │
                          │  └─ Brier score ablation testing               │
                          │                                                 │
                          │  Dashboard (FastAPI + htmx + SSE)              │
                          └─────────────────────────────────────────────────┘
```

## Trading Models

### Weather Contracts — Settlement-Aware Fair Value

Kalshi weather contracts settle on the **NWS Daily Climate Report** (CLI/DSM), which uses **local standard time** (not DST). The model tracks settlement mechanics directly:

1. **Running max/min tracking** — Maintains daily running maximum (or minimum) temperature from all observations throughout the settlement day
2. **Lock detection** — If the running max already exceeds the strike, probability locks at ~1.0 (the day's high has been recorded)
3. **METAR 6-hourly groups** — Parses `1xxxx`/`2xxxx` remark groups that feed directly into the Daily Climate Report
4. **HRRR forecast blending** — High-resolution (15-min) HRRR forecasts from Open-Meteo for remaining-day excursion probability
5. **C-to-F rounding ambiguity** — METAR reports Celsius; CLI reports Fahrenheit. Near threshold boundaries, the integer rounding creates settlement ambiguity that the model quantifies
6. **Gaussian diffusion ensemble** — Physics, climatology, and trend components with station-specific sigma from historical observations

Component weights: 35% physics, 25% HRRR, 20% trend, 20% climatology (when HRRR available).

### Crypto Contracts — Shadow RTI Estimation

Kalshi BTC contracts settle to the **CF Benchmarks Real-Time Index** (CFB RTI) — a 60-second weighted average from constituent exchanges (Coinbase, Bitstamp, Kraken, etc.), not Binance spot:

1. **Shadow RTI** — Weighted average of Coinbase (0.6) and Binance (0.4) spot prices as a proxy for the actual RTI
2. **Gaussian probability** — N(d2) model using shadow RTI, time-scaled volatility, and the contract strike
3. **Basis signal** — Perpetual futures vs spot basis indicates directional sentiment
4. **Funding rate signal** — Positive funding (longs pay shorts) signals bullish market structure
5. **Deribit DVOL** — Market-implied volatility from the BTC volatility index, preferred over realized vol when available
6. **RTI averaging dampening** — Near expiry, the 60-second averaging window smooths tail risk, pulling extreme probabilities toward 0.5

### Shared Signal Logic

- Spread-adjusted edge with wide-spread penalty (15% discount above 10% spread)
- Kelly criterion sizing using estimated fill price (best ask for YES, best bid for NO)
- Signal cooldown (300s per ticker) to prevent duplicate entries
- Exit signals when edge flips below -3%

## Project Structure

```
Tradebot/
├── config/                       # Environment configuration
├── docker/                       # Docker Compose (Postgres, Redis, NATS)
├── migrations/                   # SQL migrations (001-012)
│   ├── 009_contract_rules.sql    # Contract settlement rules
│   ├── 010_weather_sources.sql   # METAR observations, HRRR forecasts
│   ├── 011_crypto_sources.sql    # Multi-exchange crypto ticks
│   └── 012_replay_tables.sql     # Event capture + model evaluations
├── python/
│   ├── rules/                    # Contract rules & settlement mapping
│   │   ├── resolver.py           # ContractRulesResolver (DB-cached)
│   │   ├── ticker_parser.py      # Structured ticker format parser
│   │   ├── timezone.py           # DST-aware day boundary computation
│   │   └── discover.py           # One-time ticker format cataloger
│   ├── data/                     # Data source clients
│   │   ├── aviationweather.py    # METAR fetcher (6-hr max/min groups)
│   │   ├── open_meteo.py         # HRRR forecast fetcher
│   │   ├── binance_ws.py         # BTC spot WebSocket feed
│   │   ├── mesonet.py            # ASOS weather observations
│   │   └── kalshi_history.py     # Historical settlement data
│   ├── models/                   # Fair-value engines
│   │   ├── weather_fv.py         # Settlement-aware weather model
│   │   ├── crypto_fv.py          # Shadow RTI crypto model
│   │   ├── rounding.py           # METAR C→F rounding ambiguity
│   │   ├── physics.py            # Gaussian diffusion ensemble
│   │   └── binary_option.py      # Black-Scholes binary options
│   ├── signals/                  # Signal evaluation & publishing
│   │   ├── weather.py            # Weather signal evaluator
│   │   ├── crypto.py             # Crypto signal evaluator
│   │   ├── publisher.py          # NATS + DB + Redis publisher
│   │   └── types.py              # Pydantic schemas
│   ├── collector/daemon.py       # Data collection (ASOS, METAR, HRRR, BTC, Kalshi)
│   ├── evaluator/daemon.py       # Signal evaluation loop (10s cycle)
│   ├── backtester/
│   │   ├── engine.py             # Backtesting engine
│   │   └── replay.py             # Source attribution replay
│   ├── dashboard/                # FastAPI + htmx live UI
│   └── tests/                    # 199 Python tests
├── rust/
│   ├── src/
│   │   ├── main.rs               # Entry point, feed orchestration
│   │   ├── execution.rs          # NATS consumer, order execution
│   │   ├── orderbook_feed.rs     # WS → OrderbookManager → Redis
│   │   ├── config.rs             # Environment configuration
│   │   ├── feeds/                # External exchange WebSocket feeds
│   │   │   ├── coinbase.rs       # Coinbase BTC-USD level2
│   │   │   ├── binance_futures.rs # Binance BTCUSDT perp/mark/funding
│   │   │   └── deribit.rs        # Deribit DVOL index
│   │   └── kalshi/               # Kalshi exchange integration
│   │       ├── websocket.rs      # Orderbook + trade + ticker channels
│   │       ├── orderbook.rs      # DashMap in-memory orderbook
│   │       ├── trade_tape.rs     # Bounded trade buffer + metrics
│   │       ├── auth.rs           # RSA-PSS request signing
│   │       └── client.rs         # REST API client
│   └── Cargo.toml                # 24 Rust tests
└── justfile                      # Task runner
```

## Quick Start

```bash
# 1. Infrastructure
just db-up                     # Start Postgres, Redis, NATS

# 2. Configure and migrate
cp config/.env.example .env    # Fill in Kalshi API key + credentials
just migrate                   # Run SQL migrations (001-012)

# 3. Start data collection
just collector                 # ASOS, METAR, HRRR, BTC, market snapshots

# 4. Run signal evaluator
cd python && python -m evaluator.daemon

# 5. Start Rust execution engine
just dev                       # Kalshi WS + crypto feeds + order execution

# 6. Dashboard
cd python && python -m dashboard.app   # Live UI on :8050

# 7. Run tests
just test-all                  # 24 Rust + 199 Python = 223 tests
```

## Data Sources

| Source | Type | Transport | Update Frequency | Used For |
|--------|------|-----------|-----------------|----------|
| Kalshi orderbook | Market | Rust WebSocket | Real-time | Bid/ask/depth, trade tape |
| Kalshi ticker | Market | Rust WebSocket | Real-time | Volume, OI, market status |
| Coinbase (BTC-USD) | Crypto | Rust WebSocket | Real-time | Shadow RTI constituent |
| Binance Futures (BTCUSDT) | Crypto | Rust WebSocket | Real-time | Perp price, funding, basis |
| Deribit DVOL | Crypto | Rust WebSocket (optional) | Real-time | Implied volatility |
| Binance spot (BTC) | Crypto | Python WebSocket | 1s | Spot price, realized vol |
| Iowa Mesonet (ASOS) | Weather | Python REST | 60s | Temperature, wind, precip |
| AviationWeather (METAR) | Weather | Python REST | 60s | 6-hr max/min groups |
| Open-Meteo (HRRR) | Weather | Python REST | 300s | 15-min forecast temps |
| Kalshi API | Market | Python REST | 60s | Settlement history, prices |

## Redis Key Structure

```
orderbook:{ticker}          # Kalshi book state (from Rust, 500ms flush)
crypto:coinbase             # Coinbase BTC-USD spot/bid/ask
crypto:binance_futures      # Binance perp/mark/funding/OBI
crypto:deribit_dvol         # Deribit BTC volatility index
model_state:{ticker}        # Model output for dashboard
feed:status:{ticker}        # Feed health/staleness
```

## Configuration

Key environment variables (see `config/.env.example` for full list):

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL (TimescaleDB) connection | required |
| `REDIS_URL` | Redis for state cache | `redis://localhost:6379` |
| `NATS_URL` | NATS messaging | `nats://localhost:4222` |
| `KALSHI_API_KEY` | Kalshi API key | required |
| `KALSHI_PRIVATE_KEY_PATH` | RSA private key for signing | required |
| `PAPER_MODE` | Paper trading (no real orders) | `true` |
| `MAX_TRADE_SIZE_CENTS` | Per-order limit | `2500` ($25) |
| `MAX_DAILY_LOSS_CENTS` | Daily stop-loss | `10000` ($100) |
| `MAX_POSITIONS` | Max concurrent positions | `5` |
| `KELLY_FRACTION_MULTIPLIER` | Kelly scaling factor | `0.25` |
| `ENABLE_COINBASE` | Coinbase feed | `false` |
| `ENABLE_BINANCE_FUTURES` | Binance futures feed | `false` |
| `ENABLE_DERIBIT` | Deribit DVOL feed | `false` |
| `DISCORD_WEBHOOK_URL` | Alert notifications | (optional) |

## Development

```bash
just test-python     # Python tests (pytest, 199 tests)
just test            # Rust tests (cargo test, 24 tests)
just test-all        # Both (223 tests)
just fmt             # Format Rust code
just clippy          # Rust lints
```

## Tech Stack

- **Rust**: tokio, tokio-tungstenite, fred (Redis), async-nats, sqlx, serde, tracing
- **Python**: asyncio, asyncpg, httpx, pydantic, FastAPI, structlog, pytest
- **Infrastructure**: PostgreSQL/TimescaleDB, Redis, NATS, Docker Compose

## License

Private repository. All rights reserved.
