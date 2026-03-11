# Tradebot — Algorithmic Trading Bot for Kalshi Prediction Markets

![Tests](https://img.shields.io/badge/tests-354-brightgreen)
![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)
![Python](https://img.shields.io/badge/python-3.11%2B-blue)
![License](https://img.shields.io/badge/license-Apache%202.0-blue)
![Exchange](https://img.shields.io/badge/exchange-Kalshi-purple)

Algorithmic trading system for [Kalshi](https://kalshi.com) prediction markets. Trades **weather temperature contracts** and **Bitcoin crypto binary options** using settlement-aware fair-value models, real-time WebSocket feeds, and automated order execution.

Built with **Rust** (low-latency execution, exchange feeds) and **Python** (signal generation, fair-value models), connected via NATS messaging and Redis state cache.

> **438 tests** (116 Rust + 322 Python) | Paper trading mode for safe development

---

## Why This Project?

Most trading bots follow price action. Tradebot takes a fundamentally different approach — it models **how contracts actually settle** and trades when the market price diverges from fair value.

- **Settlement-aware models** — Weather contracts settle on the NWS Daily Climate Report; crypto contracts settle on the CF Benchmarks Real-Time Index. The models replicate these exact mechanics rather than following price trends
- **Two asset classes** — Weather temperature contracts and Bitcoin binary options, each with specialized fair-value engines
- **Rust + Python split architecture** — Rust for sub-millisecond feed processing and order execution; Python for weather signal generation, backtesting, and analytics
- **Paper trading first** — Kalshi demo environment support, parameter sweep framework, walk-forward optimization, and Brier score validation before risking capital
- **Full observability** — Live htmx dashboard, P&L attribution with model component tracking, per-strategy Brier scoring, and settlement summary analytics

*Keywords: prediction markets, event contracts, fair value, Kelly criterion, low-latency WebSocket, algorithmic trading, binary options*

---

## Key Features

- **Settlement-aware pricing models** — Maps directly to how Kalshi contracts settle (CFB RTI for crypto, NWS Daily Climate Reports for weather)
- **Multi-exchange crypto feeds** — Coinbase, Binance Spot, Binance Futures, Deribit DVOL via persistent WebSocket connections in Rust
- **Dynamic venue weighting** — Volume-based, staleness-aware, outlier-detecting RTI estimation
- **Weather fair-value engine** — Running max/min tracking, METAR 6-hourly groups, HRRR forecast blending, C-to-F rounding ambiguity with boundary probability
- **Station-specific calibration** — Per-(station, month, hour) sigma, HRRR bias correction, skill-weighted ensemble blending
- **Kalshi microstructure layer** — Trade tape aggressiveness, spread regime detection, depth imbalance adjustments
- **Order state machine** — Full 10-state lifecycle tracking with restart recovery and kill switch integration
- **Risk management** — Kelly criterion sizing, max position limits, daily loss stops, spread-adjusted edge thresholds
- **Per-strategy analytics** — Brier scoring, P&L attribution with model component tracking, calibration dashboard
- **Parameter sweep framework** — Grid search over model hyperparameters with walk-forward optimization
- **Dynamic WS subscription** — Contract discovery drives automatic orderbook feed subscriptions
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

**Weather contracts** settle on the NWS Daily Climate Report using an 8-step pipeline: running max/min tracking, lock detection, METAR 6-hourly group parsing, HRRR forecast blending, C-to-F rounding ambiguity modeling, station-specific calibration, source conflict detection, and Gaussian diffusion ensemble with station-calibrated weights.

**Crypto contracts** settle on the CF Benchmarks Real-Time Index using a 7-step pipeline: dynamic RTI estimation with volume weighting, N(d2) Gaussian probability, Levy averaging near expiry, basis and funding rate signals, Deribit DVOL implied volatility, and microstructure adjustments from trade tape analysis.

Both models use spread-adjusted edge filtering, Kelly criterion sizing, signal cooldowns, and exit signals on edge reversal.

See [docs/trading-models.md](docs/trading-models.md) for the full step-by-step breakdown.

## Quick Start

### Docker (recommended)

```bash
just db-up                     # Start Postgres/TimescaleDB, Redis, NATS
just up                        # Start Rust binary + all services
just dashboard                 # Live UI on :8050
```

### Local Development

```bash
# 1. Start infrastructure
just db-up                     # PostgreSQL, Redis, NATS via Docker Compose

# 2. Configure
cp config/.env.example .env    # Fill in Kalshi API key + credentials

# 3. Run migrations
just migrate                   # SQL migrations (000-018)

# 4. Start data collection
just collector                 # ASOS, METAR, HRRR, market snapshots

# 5. Run weather signal evaluator
just evaluator                 # Weather evaluation loop (10s cycle)

# 6. Start Rust execution engine
just dev                       # Kalshi WS + crypto feeds + order execution
just grafana                   # Grafana dashboards on :3033

# 7. Dashboard
just dashboard                 # Live UI on :8050
```

## Project Structure

```
Tradebot/
├── config/                       # Environment configuration
│   ├── .env.example              # Template with all env vars
│   └── kalshi_dev.pem            # RSA private key (gitignored in prod)
├── docker/                       # Docker Compose (Postgres, Redis, NATS)
│   └── docker-compose.yml
├── docs/                         # Architecture & reference docs
│   ├── build-plans/              # Phase 0-6 implementation plans
│   ├── trading-models.md         # Full model documentation
│   ├── configuration.md          # Environment variable reference
│   ├── redis-keys.md             # Redis key structure
│   ├── sql-reference.md          # SQL query reference
│   ├── data_pipeline_upgrade.md  # Settlement-focused architecture
│   └── improvements.md           # Original improvement roadmap
├── migrations/                   # SQL migrations (000-018)
│   ├── 009_contract_rules.sql    # Contract settlement rules
│   ├── 010_weather_sources.sql   # METAR observations, HRRR forecasts
│   ├── 011_crypto_sources.sql    # Multi-exchange crypto ticks
│   ├── 012_replay_tables.sql     # Event capture + model evaluations
│   ├── 013_paper_trades.sql      # Paper trading audit trail
│   ├── 014_order_state_tracking.sql  # Order state machine
│   ├── 015_station_calibration.sql   # Per-station model calibration
│   ├── 016_phase5_tables.sql     # strategy_performance, dead_letters, reconciliation
│   ├── 017_model_components.sql  # P&L attribution JSONB
│   └── 018_backtest_tables.sql   # backtest_runs, daily_settlement_summary
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
│   ├── analytics/                # Performance analytics
│   │   ├── aggregator.py         # Per-strategy analytics & Brier scoring
│   │   └── settlement_summary.py # Daily settlement summary aggregation
│   ├── collector/daemon.py       # Data collection (ASOS, METAR, HRRR, Kalshi)
│   ├── evaluator/daemon.py       # Weather signal evaluation loop (10s cycle)
│   ├── sync_contracts.py         # Kalshi contract sync (active + settled)
│   ├── backtester/               # Backtesting & optimization
│   │   ├── engine.py             # Backtesting engine
│   │   ├── sweep.py              # Parameter sweep + walk-forward optimization
│   │   └── replay.py             # Brier score ablation replay
│   ├── dashboard/                # FastAPI + htmx live UI
│   │   ├── app.py                # SSE server (port 8050)
│   │   ├── templates/            # htmx templates (index, calibration)
│   │   └── static/style.css      # Terminal-style CSS
│   └── tests/                    # 242 Python tests
├── rust/
│   └── src/
│       ├── main.rs               # Entry point, feed orchestration
│       ├── config.rs             # Environment configuration (incl. RTI params)
│       ├── execution.rs          # NATS consumer, order execution
│       ├── order_manager.rs      # Order state machine (10 states) + reconciliation
│       ├── crypto_evaluator.rs   # Event-driven crypto evaluation + microstructure
│       ├── crypto_fv.rs          # Shadow RTI, N(d2), Levy averaging, basis/funding
│       ├── crypto_state.rs       # In-process state (RwLock) + dynamic venue weights
│       ├── orderbook_feed.rs     # Kalshi WS → OrderbookManager → Redis
│       ├── dashboard.rs          # Axum HTTP dashboard server
│       ├── kill_switch.rs        # Emergency trading halt (per-strategy)
│       ├── feed_health.rs        # Per-feed health scoring (0.0-1.0)
│       ├── clock.rs              # Clock discipline (HTTP Date header sync)
│       ├── dead_letter.rs        # Dead-letter handling (NATS + DB persistence)
│       ├── contract_discovery.rs # Contract enumeration from DB
│       ├── integration_tests.rs  # Integration test scenarios
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
├── justfile                      # Task runner (just dev, just test-all, etc.)
├── CONTRIBUTING.md               # Contribution guidelines
└── CLAUDE.md                     # AI-assisted development guide
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
| 5.1 | Per-strategy analytics & Brier scoring | Complete |
| 5.2 | Calibration dashboard | Complete |
| 5.3 | P&L attribution with model_components JSONB | Complete |
| 5.4 | Reconciliation loop | Complete |
| 5.5 | Clock discipline (HTTP Date header sync) | Complete |
| 5.6 | Dead-letter handling (NATS + DB persistence) | Complete |
| 5.7 | Integration tests (8 scenarios) | Complete |
| 5.8 | Per-feed health scoring (granular 0.0-1.0) | Complete |
| 6.1 | Parameter sweep framework, settlement summary, collector enhancements | Complete |
| 7 | Calibration agent & prediction feedback loop | Complete |
| 7.3a | Price momentum signal | Complete |
| 7.3c | Volume surge detection | Complete |
| 7.3d | OI delta tracking | Complete |
| 7.5 | Edge trajectory tracking | Complete |
| 8 | Advanced backtesting & adaptive calibration | Complete |
| 8.1 | Crypto threshold sweep | Complete |
| 8.2 | Transaction costs | Complete |
| 8.3 | Advanced backtest metrics | Complete |
| 8.4 | Multi-signal evaluation | Complete |
| 8.5 | Parallel parameter sweep | Complete |
| 8.6 | Replay engine with source ablation | Complete |
| 8.7 | Comprehensive backtester tests | Complete |
| 9.0 | Grafana observability (dashboards, alerts, decision logging) | Complete |

### What's Next

- Feature development based on operational learnings
- Performance optimization and latency reduction

## Development Commands

```bash
# Testing
just test              # Rust tests (116 tests)
just test-python       # Python tests (322 tests)
just test-all          # Both (438 tests)

# Code quality
just fmt               # Format Rust code
just fmt-check         # Check Rust formatting
just clippy            # Rust lints

# Contract sync
just sync-contracts    # Sync active + settled contracts from Kalshi
just sync-active       # Active contracts only
just sync-loop         # Continuous sync every 5 minutes

# Backtesting & optimization
just sweep 2024-01-01 2024-06-30           # Parameter grid search
just walk-forward 2024-01-01 2024-12-31    # Walk-forward optimization
just leaderboard                            # Best backtest runs
just settlement-summary                     # Aggregate settlement data

# Database
just db-shell          # Open psql shell
just migrate           # Run SQL migrations

# Diagnostics
just health            # Check system health endpoint
just logs              # Follow tradebot logs
just ps                # Docker container status
```

## Tech Stack

- **Rust**: tokio, tokio-tungstenite, fred (Redis), async-nats, sqlx, rsa (pure-Rust signing), dashmap, axum, tracing
- **Python**: asyncio, asyncpg, httpx, pydantic, FastAPI, structlog, pytest
- **Infrastructure**: PostgreSQL 17/TimescaleDB, Redis 7, NATS 2 (JetStream), Docker Compose, just

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for setup instructions and guidelines.

## License

Licensed under the [Apache License 2.0](LICENSE).
