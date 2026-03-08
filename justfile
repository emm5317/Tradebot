# Infrastructure
db-up:
    docker compose -f docker/docker-compose.yml up -d

db-down:
    docker compose -f docker/docker-compose.yml down

db-reset:
    docker compose -f docker/docker-compose.yml down -v
    docker compose -f docker/docker-compose.yml up -d

# Migrations (requires sqlx-cli: cargo install sqlx-cli)
migrate:
    cd rust && sqlx migrate run --source ../migrations/

# Development
dev:
    cd rust && cargo run

build:
    cd rust && cargo build

# Testing
test:
    cd rust && cargo test

test-python:
    cd python && python -m pytest tests/ -v

test-all: test test-python

# Code quality
fmt:
    cd rust && cargo fmt

fmt-check:
    cd rust && cargo fmt --check

clippy:
    cd rust && cargo clippy -- -D warnings

# Cleanup
clean:
    cd rust && cargo clean

# Data collector
collector:
    cd python && python -m collector.daemon

# Signal evaluation loop
evaluator:
    cd python && python -m evaluator.daemon

# Terminal-style dashboard
dashboard:
    cd python && python -m dashboard.app

# Backtesting
backtest start end:
    cd python && python -m backtester.engine --start {{start}} --end {{end}}

# Historical data import (ASOS observations + Kalshi settlements)
import-history months="6":
    cd python && python -m data.historical_import --months {{months}}

import-asos months="6":
    cd python && python -m data.historical_import --asos-only --months {{months}}

import-kalshi months="6":
    cd python && python -m data.historical_import --kalshi-only --months {{months}}

# Calibration analysis (runs backtest + writes to calibration table + analyzes)
calibrate start end:
    cd python && python -m backtester.calibration --start {{start}} --end {{end}}

# Ensemble weight optimization (grid search + cross-validation)
optimize start end:
    cd python && python -m backtester.optimize --start {{start}} --end {{end}}

optimize-fine start end:
    cd python && python -m backtester.optimize --start {{start}} --end {{end}} --granularity fine --folds 5

# Full pipeline: import → calibrate → optimize
tune start end months="6":
    just import-history {{months}}
    just calibrate {{start}} {{end}}
    just optimize {{start}} {{end}}

# Diagnostics
health:
    curl -s localhost:8050/api/health | jq .
