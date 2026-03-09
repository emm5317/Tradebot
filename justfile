set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

compose := "docker compose -f docker/docker-compose.yml"

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

# ── Backtesting ─────────────────────────────────────────────

backtest start end:
    cd python; python -m backtester.engine --start {{start}} --end {{end}}

# ── Diagnostics ─────────────────────────────────────────────

health:
    curl -s localhost:8050/api/health | jq .

logs:
    {{compose}} logs -f tradebot

logs-all:
    {{compose}} logs -f

ps:
    {{compose}} ps
