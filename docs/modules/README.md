# Backend Module Plans — Index

Detailed module-by-module implementation plans for the Tradebot backend. Each document contains specifications, code snippets, package recommendations, and acceptance criteria.

## Documents

| Module | Document | Description |
|--------|----------|-------------|
| BE-1 | [Foundation](BE-01_FOUNDATION.md) | Docker (TimescaleDB, Redis, NATS), migrations, config, logging |
| BE-2 | [Kalshi Client](BE-02_KALSHI_CLIENT.md) | RSA auth, REST API, WebSocket, orderbook state |
| BE-3 | [Data Layer](BE-03_DATA_LAYER.md) | ASOS fetcher, Binance feed, collector daemon, historical pull |
| BE-4 | [Signal Engine](BE-04_SIGNAL_ENGINE.md) | Ensemble weather model, Black-Scholes, evaluators, publisher |
| BE-5 | [Execution Engine](BE-05_EXECUTION_ENGINE.md) | Risk manager, circuit breaker, Kelly sizing, market+limit orders |
| BE-6 | [Scanner + Metrics](BE-06_SCANNER_METRICS.md) | Settlement scanner (BTreeMap), latency instrumentation |
| BE-7 | [UI Backend](BE-07_UI_BACKEND.md) | Axum API endpoints, WebSocket push, health checks |
| BE-8 | [Backtest + Calibration](BE-08_BACKTEST_CALIBRATION.md) | Weather/crypto backtests, calibration evaluator, drift monitor |
| BE-9 | [Integration](BE-09_INTEGRATION.md) | E2E paper trading, reconciliation, stress test, dry-run replay |
| BE-10 | [Production](BE-10_PRODUCTION.md) | Graceful shutdown, crash recovery, live config, alerts, runbook |

## Cross-Cutting Analysis

See [PLAN_ANALYSIS.md](../PLAN_ANALYSIS.md) for:
- 7 high-impact improvements over the original plan
- Recommended technology stack (Rust crates + Python packages)
- Risk assessment of the current plan

## Dependency Graph

```
BE-1 (Foundation)
  ├─► BE-2 (Kalshi Client)
  │     ├─► BE-2.4 (Orderbook) ─► BE-5.5 (Limit Orders)
  │     └─► BE-6.1 (Scanner)
  ├─► BE-3 (Data Layer)
  │     └─► BE-4 (Signal Engine) ─► BE-5.7 (Consumer)
  └─► BE-5 (Execution)
        ├─► BE-6.2 (Metrics)
        ├─► BE-7 (UI Backend)
        └─► BE-8 (Backtest + Calibration)

BE-9 (Integration) requires all of BE-1 through BE-8
BE-10 (Hardening) requires BE-9
```

## Key Changes from Original Plan

| Area | Original | Improved |
|------|----------|----------|
| Messaging | Redis Streams | **NATS JetStream** — lower latency, built-in delivery guarantees |
| Database | Plain PostgreSQL | **TimescaleDB** — hypertables, continuous aggregates, compression |
| Weather model | Single Gaussian (σ=0.3) | **Ensemble** — physics + climatology + trend extrapolation |
| Position limits | Simple count (max 4) | **Correlation-aware** — per-city, per-asset-class limits |
| Order types | Market only (initially) | **Market + limit** with orderbook-driven strategy selection |
| Exit strategy | Hold to settlement | **Continuous re-evaluation** — exit if edge flips |
| Alerting | Logs only | **Discord webhooks** for critical events |
| Health monitoring | None | **`/api/health`** endpoint with component status |
| Idempotency | None | **Deterministic order IDs** prevent duplicates on crash |
| Python packages | pip + dataclasses | **uv + pydantic v2** — faster installs, runtime validation |
| Data analytics | pandas | **polars** — 10-100x faster for backtesting |
| Redis client | redis-rs | **fred** — connection pooling, pipeline batching |
| WebSocket | tokio-tungstenite | **tokio-tungstenite** confirmed — thread-safe, `fastwebsockets` has soundness issues |
