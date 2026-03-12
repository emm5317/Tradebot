# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.2] - 2026-03-11

### Fixed

- Orderbook delta price complement bug — "no"-side deltas now correctly apply `100 - price` before writing to ask book
- N(d2) model overconfidence on bracket contracts — added kurtosis tail correction and probability floor/ceiling (0.02-0.98)

### Added

- Max edge filter (`CRYPTO_MAX_EDGE=0.25`) rejects signals with unrealistically large edge as model miscalibration
- Expanded parameter sweep grid with `max_edge` dimension

### Changed

- Lowered crypto evaluation thresholds: min_edge 0.06→0.03, min_kelly 0.04→0.02, min_confidence 0.50→0.40

## [0.12.1] - 2026-03-11

### Added

- Prometheus metrics: eval_duration_seconds, eval_total, order_latency_seconds, orders_total, feed_health_score, decision_log_channel_usage
- Health endpoints: /health/live (liveness), /health/ready (readiness with DB/Redis/NATS/feed checks), /metrics (Prometheus)
- Discord webhook alerts for critical task failures
- Supervisor task activity gauge

## [0.12.0] - 2026-03-10

### Added

- Lock poison recovery via RwLockExt trait (read_or_recover, write_or_recover)
- Graceful shutdown with order draining (5-stage: kill switch → cancel → drain → confirm → cleanup)
- Task supervision with criticality levels (Critical vs NonCritical)
- Batch decision log writer with mpsc channel (1024 buffer, 100-entry batches, 1s flush)
- Parallel Python I/O in collector and calibrator daemons

### Fixed

- Eliminated 12 potential lock poison panics across 4 modules

## [0.11.0] - 2026-03-11

### Added

- Bloomberg terminal dashboard — 6-page FastAPI + htmx UI (MAIN, SGNL, EXEC, ANAL, RISK, WEAT)
- IBM Plex Mono amber-on-black terminal theme with Chart.js visualizations
- SSE real-time updates across all pages
- 15+ new API endpoints for dashboard data

## [0.10.1] - 2026-03-10

### Fixed

- Crypto model profitability: NO fill price, negative spread handling, fill price bounds, risk/reward guard

## [0.9.0] - 2026-03-09

### Added

- Grafana observability: 4 auto-provisioned dashboards, 5 alert rules (Discord)
- decision_log table for every evaluation outcome
- feed_health_log table for 60s health snapshots
- Decision trace instrumentation in Rust crypto evaluator (5 rejection + 1 success path)

## [0.8.0] - 2026-03-09

### Added

- Advanced backtesting: transaction costs, 8 advanced metrics (log-loss, Sharpe, Sortino, max drawdown, ECE, profit factor)
- Replay engine with source ablation and Brier score attribution
- Parallel parameter sweep with --workers flag
- Multi-signal evaluation per contract
- 81 new Python tests

## [0.7.0] - 2026-03-09

### Added

- Calibration agent daemon with 8 hourly jobs (Brier, slippage, HRRR skill, parameter optimization)
- Price momentum, volume surge, OI delta microstructure signals
- Edge trajectory tracking with should_wait() deferral
- Confidence-scaled Kelly sizing
- Evaluator hot-reload (15-min refresh cycle)

## [0.6.1] - 2026-03-09

### Added

- Parameter sweep framework for weather and crypto thresholds
- Daily settlement summary aggregation
- Collector enhancements (crypto_ticks, settlement aggregation)

## [0.5.0] - 2026-03-08

### Added

- Per-strategy analytics and Brier scoring
- P&L attribution with model_components JSONB
- Clock discipline (HTTP Date header sync)
- Dead-letter handling (NATS + DB persistence)
- Integration tests (8 scenarios)
- Per-feed health scoring (0.0-1.0 granular)

[Unreleased]: https://github.com/emm5317/Tradebot/compare/v0.12.2...HEAD
[0.12.2]: https://github.com/emm5317/Tradebot/compare/v0.12.1...v0.12.2
[0.12.1]: https://github.com/emm5317/Tradebot/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/emm5317/Tradebot/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/emm5317/Tradebot/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/emm5317/Tradebot/compare/v0.9.0...v0.10.1
[0.9.0]: https://github.com/emm5317/Tradebot/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/emm5317/Tradebot/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/emm5317/Tradebot/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/emm5317/Tradebot/compare/v0.5.0...v0.6.1
[0.5.0]: https://github.com/emm5317/Tradebot/releases/tag/v0.5.0
