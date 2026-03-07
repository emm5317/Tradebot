# tradebot justfile
# Run `just --list` to see all available recipes

# Default recipe
default:
    @just --list

# --- Infrastructure ---

# Start PostgreSQL + Redis via Docker Compose
db-up:
    docker compose -f docker/docker-compose.yml up -d postgres redis

# Stop infrastructure
db-down:
    docker compose -f docker/docker-compose.yml down

# Run database migrations
migrate:
    cd rust && sqlx migrate run --source ../migrations

# Reset database (drop + recreate + migrate)
db-reset:
    cd rust && sqlx database drop -y && sqlx database create && sqlx migrate run --source ../migrations

# --- Rust Execution Engine ---

# Build release binary
build:
    cd rust && cargo build --release

# Build debug
build-debug:
    cd rust && cargo build

# Run all Rust tests
test-rust:
    cd rust && cargo test

# Run clippy linter
clippy:
    cd rust && cargo clippy -- -D warnings

# Format Rust code
fmt:
    cd rust && cargo fmt

# --- Python Signal Engine ---

# Create virtual environment and install dependencies
venv:
    cd python && python -m venv .venv && .venv/bin/pip install -r requirements.txt

# Run Python tests
test-python:
    cd python && .venv/bin/python -m pytest

# Run ruff linter
ruff:
    cd python && .venv/bin/ruff check .

# --- Backtesting ---

# Run weather backtest
backtest-weather:
    cd python && .venv/bin/python -m backtest.weather_backtest

# Run crypto backtest
backtest-crypto:
    cd python && .venv/bin/python -m backtest.crypto_backtest

# --- Trading ---

# Start both engines in paper mode
paper: build
    @echo "Starting paper trading mode..."
    @echo "Ensure db-up has been run and migrations are current."

# --- Combined ---

# Run all tests
test: test-rust test-python

# Run all linters
lint: clippy ruff

# Full check: lint + test
check: lint test
