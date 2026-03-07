# Tradebot task runner

default:
    @just --list

# Start local infrastructure (Postgres + Redis)
infra:
    docker compose -f docker/docker-compose.yml up -d postgres redis

# Run database migrations
migrate:
    @for f in migrations/*.sql; do echo "Applying $f ..."; psql $DATABASE_URL -f "$f"; done

# Build the Rust engine
build:
    cd rust && cargo build --release

# Run the Rust engine
run:
    cd rust && cargo run --release

# Run Rust tests
test-rust:
    cd rust && cargo test

# Install Python dependencies
pip:
    cd python && pip install -r requirements.txt

# Run Python signal scanner
signals:
    cd python && python -m signals.scanner

# Run weather backtest
backtest-weather:
    cd python && python -m backtest.weather_backtest

# Run crypto backtest
backtest-crypto:
    cd python && python -m backtest.crypto_backtest

# Start everything for paper trading
paper: infra
    @echo "Starting paper trading ..."
    TRADING_MODE=paper just run

# Lint Python code
lint:
    cd python && ruff check .

# Format Python code
fmt:
    cd python && ruff format .
