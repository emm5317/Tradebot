set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

compose := "docker compose -f docker/docker-compose.yml --env-file .env"

# ── Infrastructure ──────────────────────────────────────────

# Start all infra (postgres, redis, nats) + run migrations
db-up:
    {{compose}} up -d postgres redis nats
    {{compose}} up migrate

db-down:
    {{compose}} down

db-reset:
    {{compose}} down -v
    {{compose}} up -d postgres redis nats
    {{compose}} up migrate

# Run migrations against running postgres
migrate:
    {{compose}} up migrate

# ── Docker (full stack) ─────────────────────────────────────

# Build the tradebot image
docker-build:
    {{compose}} build tradebot

# Start everything (infra + tradebot Rust binary)
up:
    {{compose}} up -d

# Start everything and follow logs
up-logs:
    {{compose}} up

# Stop everything
down:
    {{compose}} down

# Rebuild and restart
restart:
    {{compose}} up -d --build tradebot

# ── Docker: individual services ─────────────────────────────

# Run collector inside the tradebot container
docker-collector:
    {{compose}} run --rm tradebot python -m collector.daemon

# Run evaluator inside the tradebot container
docker-evaluator:
    {{compose}} run --rm tradebot python -m evaluator.daemon

# Run dashboard inside the tradebot container
docker-dashboard:
    {{compose}} run --rm -p 8050:8050 tradebot python -m dashboard.app

# Open a shell inside the tradebot container
docker-shell:
    {{compose}} run --rm tradebot bash

# ── Local development (runs on host, talks to docker infra) ─

# Run Rust binary locally
dev:
    cd rust; cargo run

build:
    cd rust; cargo build

# Run collector locally (uses docker postgres/redis/nats on default ports)
collector:
    cd python; python -m collector.daemon

# Run evaluator locally
evaluator:
    cd python; python -m evaluator.daemon

# Run dashboard locally
dashboard:
    cd python; python -m dashboard.app

# ── Testing ─────────────────────────────────────────────────

test:
    cd rust; cargo test

test-python:
    cd python; python -m pytest tests/ -v

test-all: test test-python

# Run tests inside docker
docker-test:
    {{compose}} run --rm tradebot bash -c "cd python && python -m pytest tests/ -v"

# ── Code quality ────────────────────────────────────────────

fmt:
    cd rust; cargo fmt

fmt-check:
    cd rust; cargo fmt --check

clippy:
    cd rust; cargo clippy -- -D warnings

clean:
    cd rust; cargo clean

# ── Contract sync ───────────────────────────────────────────

# Sync active + settled contracts from Kalshi API into DB
sync-contracts:
    cd python; python -m sync_contracts

# Sync active contracts only (for paper trading)
sync-active:
    cd python; python -m sync_contracts --active

# Continuous contract sync every 5 minutes
sync-loop:
    cd python; python -m sync_contracts --active --loop 300

# ── Backtesting ─────────────────────────────────────────────

backtest start end:
    cd python; python -m backtester.engine --start {{start}} --end {{end}}

# Parameter sweep (grid search over model hyperparameters)
sweep start end:
    cd python; python -m backtester.sweep --start {{start}} --end {{end}}

sweep-crypto start end:
    cd python; python -m backtester.sweep --start {{start}} --end {{end}} --type crypto

# Walk-forward optimization (train/validate splits)
walk-forward start end window="14":
    cd python; python -m backtester.sweep --start {{start}} --end {{end}} --walk-forward {{window}}

# Print leaderboard of best backtest runs
leaderboard type="weather":
    cd python; python -m backtester.sweep --start 2000-01-01 --end 2099-01-01 --type {{type}} --leaderboard

# Settlement summary aggregation
settlement-summary:
    cd python; python -m analytics.settlement_summary

settlement-backfill days="30":
    cd python; python -m analytics.settlement_summary --backfill {{days}}

# ── Database shell ──────────────────────────────────────────

# Open psql shell inside the postgres container
db-shell:
    docker exec -it docker-postgres-1 psql -U tradebot -d tradebot

# ── Observability ──────────────────────────────────────────

# Start Grafana dashboards on :3033
grafana:
    {{compose}} up -d grafana

# Restart Grafana (picks up dashboard changes)
grafana-restart:
    {{compose}} restart grafana

# ── Diagnostics ─────────────────────────────────────────────

health:
    curl -s localhost:8050/api/health | jq .

logs:
    {{compose}} logs -f tradebot

logs-all:
    {{compose}} logs -f

ps:
    {{compose}} ps
