# Tradebot — Automated Kalshi Prediction Market Trading Bot

Algorithmic trading system for [Kalshi](https://kalshi.com) prediction markets. Trades **weather temperature contracts** and **Bitcoin crypto binary options** using settlement-aware fair-value models, real-time WebSocket feeds, and automated order execution.

Built with **Rust** (low-latency execution, exchange feeds) and **Python** (signal generation, fair-value models), connected via NATS messaging and Redis state cache.

> **328 tests** (92 Rust + 236 Python) | Paper trading mode for safe development

---

## Key Features

- **Settlement-aware pricing models** — Models map directly to how Kalshi contracts settle: CFB Real-Time Index for crypto, NWS Daily Climate Reports for weather
- **Multi-exchange crypto feeds** — Coinbase, Binance Spot, Binance Futures, Deribit DVOL via persistent WebSocket connections in Rust
- **Dynamic venue weighting** — Volume-based, staleness-aware, outlier-detecting RTI estimation replaces fixed 60/40 weights
- **Weather fair-value engine** — Running max/min tracking, METAR 6-hourly group parsing, HRRR forecast blending, C-to-F rounding ambiguity with boundary probability model
- **Station-specific calibration** — Per-(station, month, hour) sigma, HRRR bias correction, skill-weighted ensemble blending
- **Kalshi microstructure layer** — Trade tape aggressiveness, spread regime detection, depth imbalance adjustments
- **Structured contract rules** — Ticker parser extracts strike, station, settlement source from structured ticker format (no regex-on-title)
- **Real-time orderbook** — DashMap-based in-memory orderbook per ticker with trade tape, aggressiveness metrics, and stale feed detection
- **Order state machine** — Full lifecycle tracking: Pending → Submitting → Acknowledged → PartialFill → Filled, with restart recovery and kill switch integration
- **Risk management** — Kelly criterion sizing, max position limits, daily loss stops, spread-adjusted edge thresholds, rate limiting
- **Source conflict detection** — METAR/HRRR disagreement handling with automatic sigma inflation and outage policies
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
  Coinbase WS ────────────┤  CoinbaseFeed (BTC-USD level2     │            │
                          │    + market_trades for volume)     │            │
  Binance Spot WS ────────┤  BinanceSpotFeed (OHLC, vol)      ├─► Redis   │
  Binance Futures WS ─────┤  BinanceFuturesFeed (perp/mark)   │            │
  Deribit WS ─────────────┤  DeribitFeed (DVOL index)         │            │
                          │                                    │            │
                          │  CryptoState (in-process, RwLock)  │            │
                          │  ├─ Dynamic RTI (volume-weighted)  │            │
                          │  ├─ Staleness + outlier detection   │            │
                          │  └─ Reliability flag               │            │
                          │                                    │            │
                          │  CryptoEvaluator (event-driven)    │            │
                          │  ├─ Levy averaging near expiry     │            │
                          │  ├─ Microstructure adjustments     │            │
                          │  └─ Kelly sizing + edge filters    │            │
                          │                                    │            │
                          │  OrderManager (state machine)      │            │
                          │  ├─ 10-state lifecycle tracking    │            │
                          │  ├─ Partial fills, cancel/replace  │            │
                          │  └─ Kill switch integration        │            │
                          │                                    │            │
                          │  Execution Engine                  │            │
                          │  ├─ Position manager               │            │
                          │  ├─ Risk checks + rate limiting    │            │
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
                          │  │  ├─ Station-specific calibration            │
                          │  │  ├─ Source conflict / outage detection       │
                          │  │  └─ C→F rounding boundary probability       │
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
2. **Lock detection** — If the running max already exceeds the strike, probability locks at ~0.99 (the day's high has been recorded)
3. **METAR 6-hourly groups** — Parses `1xxxx`/`2xxxx` remark groups that feed directly into the Daily Climate Report
4. **HRRR forecast blending** — High-resolution (15-min) HRRR forecasts from Open-Meteo for remaining-day excursion probability, with per-station bias correction
5. **C-to-F rounding ambiguity** — METAR reports Celsius; CLI reports Fahrenheit. Near threshold boundaries, the integer rounding creates settlement ambiguity. The model computes boundary probability using a uniform distribution over the possible Fahrenheit range and identifies "safe zones" where rounding cannot affect the outcome
6. **Station-specific calibration** — Per-(station, month, hour) sigma from historical observations, HRRR skill scoring (1 - RMSE/climo_std), and optimized ensemble weights per station
7. **Source conflict detection** — When METAR and HRRR disagree by >3°F, sigma is inflated 50%. METAR outage inflates sigma 25%. Both missing yields low-confidence 0.5 probability
8. **Gaussian diffusion ensemble** — Physics, HRRR, trend, and climatology components with station-calibrated weights

Default component weights: 35% physics, 25% HRRR, 20% trend, 20% climatology (overridden by station-specific calibration when available).

### Crypto Contracts — Event-Driven Fair Value (Rust)

Kalshi BTC contracts settle to the **CF Benchmarks Real-Time Index** (CFB RTI) — a 60-second weighted average from constituent exchanges (Coinbase, Bitstamp, Kraken, etc.):

1. **Dynamic RTI estimation** — Volume-weighted average of constituent exchange spot prices with staleness detection (>5s = weight 0), outlier capping (>0.5% deviation from median = weight capped at 10%), and reliability flagging (requires 2+ healthy venues)
2. **Gaussian probability** — N(d2) model using shadow RTI, time-scaled volatility, and the contract strike
3. **Levy averaging near expiry** — Within the final 60s, the RTI averaging window dampens tail risk. Uses Levy's approximation for arithmetic average options to model effective strike shift and volatility reduction
4. **Basis signal** — Perpetual futures vs spot basis indicates directional sentiment
5. **Funding rate signal** — Positive funding (longs pay shorts) signals bullish market structure
6. **Deribit DVOL** — Market-implied volatility from the BTC volatility index, preferred over realized vol when available
7. **Microstructure adjustments** — Trade tape aggressiveness (±2%), spread regime penalties (tight: +1%, wide: -2%), depth imbalance (±2%), clamped to ±4% total

### Shared Signal Logic

- Spread-adjusted edge with wide-spread penalty (15% discount above 10% spread)
- Kelly criterion sizing using estimated fill price (best ask for YES, best bid for NO)
- Signal cooldown (crypto: 30s, weather: 120s per ticker) to prevent duplicate entries
- Exit signals when edge flips below -3%

## Project Structure

```
Tradebot/
├── config/                       # Environment configuration
│   ├── .env.example              # Template with all env vars
│   └── kalshi_dev.pem            # RSA private key (gitignored in prod)
├── docker/                       # Docker Compose (Postgres, Redis, NATS)
│   └── docker-compose.yml
├── docs/                         # Architecture docs & build plans
│   ├── build-plans/              # Phase 0-5 implementation plans
│   ├── data_pipeline_upgrade.md  # Settlement-focused architecture
│   └── improvements.md           # Original improvement roadmap
├── migrations/                   # SQL migrations (001-015)
│   ├── 009_contract_rules.sql    # Contract settlement rules
│   ├── 010_weather_sources.sql   # METAR observations, HRRR forecasts
│   ├── 011_crypto_sources.sql    # Multi-exchange crypto ticks
│   ├── 012_replay_tables.sql     # Event capture + model evaluations
│   ├── 013_paper_trades.sql      # Paper trading audit trail
│   ├── 014_order_state_tracking.sql  # Order state machine
│   └── 015_station_calibration.sql   # Per-station model calibration
├── python/
│   ├── rules/                    # Contract rules & settlement mapping
│   │   ├── resolver.py           # ContractRulesResolver (DB-cached)
│   │   ├── ticker_parser.py      # Structured ticker format parser
│   │   ├── timezone.py           # DST-aware day boundary computation
│   │   └── discover.py           # One-time ticker format cataloger
│   ├── data/                     # Data source clients
│   │   ├── aviationweather.py    # METAR fetcher (6-hr max/min groups)
│   │   ├── open_meteo.py         # HRRR forecast fetcher
│   │   ├── mesonet.py            # ASOS weather observations
│   │   └── kalshi_history.py     # Historical settlement data
│   ├── models/                   # Fair-value engines
│   │   ├── weather_fv.py         # Settlement-aware weather model
│   │   ├── crypto_fv.py          # Shadow RTI crypto model (deprecated → Rust)
│   │   ├── rounding.py           # METAR C→F rounding + boundary probability
│   │   ├── physics.py            # Gaussian ensemble + station calibration
│   │   └── binary_option.py      # Black-Scholes binary options
│   ├── signals/                  # Signal evaluation & publishing
│   │   ├── weather.py            # Weather signal evaluator
│   │   ├── crypto.py             # Crypto signal evaluator (deprecated → Rust)
│   │   ├── publisher.py          # NATS + DB + Redis publisher
│   │   ├── notifier.py           # Discord webhook alerts
│   │   ├── registry.py           # Evaluator plugin registry
│   │   ├── utils.py              # Kelly, edge, fill price utilities
│   │   └── types.py              # Pydantic schemas
│   ├── collector/daemon.py       # Data collection (ASOS, METAR, HRRR, Kalshi)
│   ├── evaluator/daemon.py       # Weather signal evaluation loop (10s cycle)
│   ├── backtester/               # Backtesting & source attribution
│   │   ├── engine.py             # Backtesting engine
│   │   └── replay.py             # Brier score ablation replay
│   ├── dashboard/                # FastAPI + htmx live UI
│   │   ├── app.py                # SSE server (port 8050)
│   │   ├── templates/index.html  # htmx live dashboard
│   │   └── static/style.css      # Terminal-style CSS
│   └── tests/                    # 236 Python tests
├── rust/
│   └── src/
│       ├── main.rs               # Entry point, feed orchestration
│       ├── config.rs             # Environment configuration (incl. RTI params)
│       ├── execution.rs          # NATS consumer, order execution
│       ├── order_manager.rs      # Order state machine (10 states)
│       ├── crypto_evaluator.rs   # Event-driven crypto evaluation + microstructure
│       ├── crypto_fv.rs          # Shadow RTI, N(d2), Levy averaging, basis/funding
│       ├── crypto_state.rs       # In-process state (RwLock) + dynamic venue weights
│       ├── orderbook_feed.rs     # Kalshi WS → OrderbookManager → Redis
│       ├── dashboard.rs          # Axum HTTP dashboard server
│       ├── kill_switch.rs        # Emergency trading halt (per-strategy)
│       ├── feed_health.rs        # Feed staleness detection
│       ├── contract_discovery.rs # Contract enumeration from DB
│       ├── feeds/                # External exchange WebSocket feeds
│       │   ├── coinbase.rs       # Coinbase BTC-USD level2 + trade volume
│       │   ├── binance_spot.rs   # Binance spot (OHLC bars, EWMA vol)
│       │   ├── binance_futures.rs # Binance BTCUSDT perp/mark/funding
│       │   └── deribit.rs        # Deribit DVOL index
│       └── kalshi/               # Kalshi exchange integration
│           ├── websocket.rs      # Orderbook + trade + ticker channels
│           ├── orderbook.rs      # DashMap in-memory orderbook
│           ├── trade_tape.rs     # Bounded trade buffer + metrics
│           ├── auth.rs           # RSA-PSS request signing (pure Rust)
│           └── client.rs         # REST API client
├── Dockerfile                    # Multi-stage Rust build
└── justfile                      # Task runner (just dev, just test-all, etc.)
```

## Quick Start

```bash
# 1. Infrastructure
just db-up                     # Start Postgres/TimescaleDB, Redis, NATS

# 2. Configure and migrate
cp config/.env.example .env    # Fill in Kalshi API key + credentials
just migrate                   # Run SQL migrations (001-015)

# 3. Start data collection
just collector                 # ASOS, METAR, HRRR, market snapshots

# 4. Run signal evaluator (weather)
just evaluator                 # Weather signal evaluation loop

# 5. Start Rust execution engine
just dev                       # Kalshi WS + crypto feeds + order execution

# 6. Dashboard
just dashboard                 # Live UI on :8050

# 7. Run tests
just test-all                  # 92 Rust + 236 Python = 328 tests
```

## Data Sources

| Source | Type | Transport | Update Frequency | Used For |
|--------|------|-----------|-----------------|----------|
| Kalshi orderbook | Market | Rust WebSocket | Real-time | Bid/ask/depth, trade tape |
| Kalshi ticker | Market | Rust WebSocket | Real-time | Volume, OI, market status |
| Coinbase (BTC-USD) | Crypto | Rust WebSocket | Real-time | RTI constituent + 5-min trade volume |
| Binance Spot (BTC) | Crypto | Rust WebSocket | Real-time | RTI constituent + EWMA/realized vol |
| Binance Futures (BTCUSDT) | Crypto | Rust WebSocket | Real-time | Perp price, funding, basis, OBI |
| Deribit DVOL | Crypto | Rust WebSocket (optional) | Real-time | Implied volatility |
| Iowa Mesonet (ASOS) | Weather | Python REST | 60s | Temperature, wind, precip |
| AviationWeather (METAR) | Weather | Python REST | 60s | 6-hr max/min groups |
| Open-Meteo (HRRR) | Weather | Python REST | 300s | 15-min forecast temps |
| Kalshi API | Market | Python REST | 60s | Settlement history, prices |

## Redis Key Structure

```
orderbook:{ticker}          # Kalshi book state (from Rust, 500ms flush)
crypto:coinbase             # Coinbase BTC-USD spot/bid/ask + trade volume
crypto:binance_spot         # Binance spot + realized/EWMA vol
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
| `ENABLE_BINANCE_SPOT` | Binance spot feed | `false` |
| `ENABLE_BINANCE_FUTURES` | Binance futures feed | `false` |
| `ENABLE_DERIBIT` | Deribit DVOL feed | `false` |
| `RTI_STALE_THRESHOLD_SECS` | Venue staleness cutoff | `5` |
| `RTI_OUTLIER_THRESHOLD_PCT` | Outlier deviation cap | `0.5` |
| `RTI_MIN_VENUES` | Min healthy venues for reliable RTI | `2` |
| `DISCORD_WEBHOOK_URL` | Alert notifications | (optional) |

## Development

```bash
just test-python     # Python tests (pytest, 236 tests)
just test            # Rust tests (cargo test, 92 tests)
just test-all        # Both (328 tests)
just fmt             # Format Rust code
just clippy          # Rust lints
just health          # Check system health endpoint
```

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| 0 | Stabilization (Binance spot, kill switches, paper mode, feed health) | Complete |
| 1 | Crypto architecture refactor (CryptoState, inline N(d2), demote Python) | Complete |
| 2 | Order state machine (lifecycle tracking, partial fills, reconciliation) | Complete |
| 3 | Event-driven crypto evaluation + Docker + dashboard | Complete |
| 4.1 | Levy approximation for RTI averaging near expiry | Complete |
| 4.2 | Dynamic RTI venue weighting (volume, staleness, outlier) | Complete |
| 4.3 | Kalshi microstructure layer (trade tape, spread, depth) | Complete |
| 4.4 | Complete Rust crypto FV port + parity tests | Complete |
| 4.5 | Station-specific calibration (sigma, HRRR bias/skill, weights) | Complete |
| 4.6 | Source conflict and outage policy | Complete |
| 4.7 | Rounding ambiguity hardening (boundary probability, safe zones) | Complete |
| 5 | Calibration dashboard, P&L attribution, production readiness | Planned |

## Tech Stack

- **Rust**: tokio, tokio-tungstenite, fred (Redis), async-nats, sqlx, rsa (pure-Rust signing), dashmap, axum, tracing
- **Python**: asyncio, asyncpg, httpx, pydantic, FastAPI, structlog, pytest
- **Infrastructure**: PostgreSQL 17/TimescaleDB, Redis 7, NATS 2 (JetStream), Docker Compose

## License

Private repository. All rights reserved.
