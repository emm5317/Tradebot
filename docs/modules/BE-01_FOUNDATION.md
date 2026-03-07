# BE-1: Foundation — Database + Config + Infrastructure

**Dependencies**: None (first module)
**Blocks**: Everything else
**Estimated sub-tasks**: 4
**Language**: SQL, Rust, Docker/YAML

---

## Overview

This module establishes the infrastructure every other module depends on: database, cache/messaging, configuration, and structured logging. Nothing else runs without this.

---

## BE-1.1: Docker Compose Environment

### Deliverable
`docker/docker-compose.yml` starting PostgreSQL 17 (with TimescaleDB extension), Redis 7, and NATS.

### Specification

```yaml
services:
  postgres:
    image: timescale/timescaledb:latest-pg17
    ports: ["5432:5432"]
    environment:
      POSTGRES_DB: tradebot
      POSTGRES_USER: tradebot
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-tradebot_dev}
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U tradebot"]
      interval: 5s
      retries: 5

  redis:
    image: redis:7-alpine
    ports: ["6379:6379"]
    volumes:
      - redisdata:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 5s
      retries: 5

  nats:
    image: nats:2-alpine
    ports:
      - "4222:4222"   # client
      - "8222:8222"   # monitoring
    command: ["--jetstream", "--store_dir=/data"]
    volumes:
      - natsdata:/data
    healthcheck:
      test: ["CMD", "nats-server", "--help"]
      interval: 5s
      retries: 5

volumes:
  pgdata:
  redisdata:
  natsdata:
```

### Improvement over original plan
- **TimescaleDB** instead of plain PostgreSQL — enables hypertables for time-series data
- **NATS** added — replaces Redis Streams for inter-process messaging (lower latency, built-in at-least-once delivery)
- **Redis retained** — used as fast KV cache for orderbook state, not as message broker

### Verification
- `docker compose up -d` succeeds
- `psql -h localhost -U tradebot -d tradebot -c "SELECT 1"` returns 1
- `redis-cli ping` returns PONG
- `curl http://localhost:8222/healthz` returns OK (NATS)

### Justfile recipes
```just
db-up:
    docker compose -f docker/docker-compose.yml up -d

db-down:
    docker compose -f docker/docker-compose.yml down

db-reset:
    docker compose -f docker/docker-compose.yml down -v
    docker compose -f docker/docker-compose.yml up -d
```

---

## BE-1.2: Database Schema + Migrations

### Deliverable
Migration files `001` through `007` in `migrations/`.

### Tables

**001_contracts.sql** — Market metadata
```sql
CREATE TABLE contracts (
    ticker        TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    category      TEXT NOT NULL,          -- 'weather', 'crypto'
    city          TEXT,                    -- for weather contracts
    station       TEXT,                    -- ASOS station ID
    threshold     REAL,                   -- strike value
    settlement_time TIMESTAMPTZ NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active',
    settled_yes   BOOLEAN,
    close_price   REAL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_contracts_settlement ON contracts(settlement_time);
CREATE INDEX idx_contracts_status ON contracts(status);
CREATE INDEX idx_contracts_category ON contracts(category);
```

**002_signals.sql** — Every signal generated (acted on or not)
```sql
CREATE TABLE signals (
    id              BIGSERIAL PRIMARY KEY,
    ticker          TEXT NOT NULL REFERENCES contracts(ticker),
    signal_type     TEXT NOT NULL,         -- 'weather', 'crypto'
    direction       TEXT NOT NULL,         -- 'yes', 'no'
    model_prob      REAL NOT NULL,
    market_price    REAL NOT NULL,
    edge            REAL NOT NULL,
    kelly_fraction  REAL NOT NULL,
    minutes_remaining REAL NOT NULL,
    observation_data JSONB,               -- raw observation snapshot
    acted_on        BOOLEAN NOT NULL DEFAULT false,
    rejection_reason TEXT,                 -- why not acted on (if applicable)
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_signals_created ON signals(created_at);
CREATE INDEX idx_signals_ticker ON signals(ticker);
-- Convert to hypertable for time-series performance
SELECT create_hypertable('signals', 'created_at', if_not_exists => TRUE);
```

**003_orders.sql** — Every order placed
```sql
CREATE TABLE orders (
    id              BIGSERIAL PRIMARY KEY,
    kalshi_order_id TEXT UNIQUE,
    idempotency_key TEXT UNIQUE NOT NULL,  -- prevents duplicate orders on crash
    signal_id       BIGINT REFERENCES signals(id),
    ticker          TEXT NOT NULL REFERENCES contracts(ticker),
    direction       TEXT NOT NULL,
    order_type      TEXT NOT NULL,         -- 'market', 'limit'
    size_cents      INTEGER NOT NULL,
    limit_price     REAL,
    fill_price      REAL,
    status          TEXT NOT NULL DEFAULT 'pending',  -- pending, filled, cancelled, unknown
    outcome         TEXT DEFAULT 'pending',           -- pending, win, loss
    pnl_cents       INTEGER,
    latency_ms      REAL,                 -- signal-to-fill latency
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    filled_at       TIMESTAMPTZ,
    settled_at      TIMESTAMPTZ
);

CREATE INDEX idx_orders_created ON orders(created_at);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_ticker ON orders(ticker);
```

**004_daily_summary.sql** — Aggregated daily stats
```sql
CREATE TABLE daily_summary (
    date            DATE PRIMARY KEY,
    total_signals   INTEGER NOT NULL DEFAULT 0,
    total_orders    INTEGER NOT NULL DEFAULT 0,
    wins            INTEGER NOT NULL DEFAULT 0,
    losses          INTEGER NOT NULL DEFAULT 0,
    gross_pnl_cents INTEGER NOT NULL DEFAULT 0,
    fees_cents      INTEGER NOT NULL DEFAULT 0,
    net_pnl_cents   INTEGER NOT NULL DEFAULT 0,
    max_drawdown    INTEGER NOT NULL DEFAULT 0,
    avg_edge        REAL,
    avg_latency_ms  REAL,
    notes           TEXT
);
```

**005_observations.sql** — ASOS readings and BTC snapshots
```sql
CREATE TABLE observations (
    id              BIGSERIAL,
    source          TEXT NOT NULL,         -- 'asos', 'binance'
    station         TEXT,                  -- ASOS station ID (null for crypto)
    observed_at     TIMESTAMPTZ NOT NULL,
    temperature_f   REAL,
    wind_speed_kts  REAL,
    wind_gust_kts   REAL,
    precip_inch     REAL,
    btc_spot        REAL,
    btc_vol_30m     REAL,                 -- 30-min annualized realized vol
    raw_data        JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, observed_at)
);

SELECT create_hypertable('observations', 'observed_at', if_not_exists => TRUE);
CREATE INDEX idx_obs_station_time ON observations(station, observed_at);
CREATE INDEX idx_obs_source ON observations(source);
```

**006_market_snapshots.sql** — Kalshi price snapshots near settlement
```sql
CREATE TABLE market_snapshots (
    id              BIGSERIAL,
    ticker          TEXT NOT NULL,
    yes_price       REAL NOT NULL,
    no_price        REAL NOT NULL,
    spread          REAL NOT NULL,
    best_bid        REAL,
    best_ask        REAL,
    bid_depth       INTEGER,              -- total contracts on bid side
    ask_depth       INTEGER,
    minutes_to_settlement REAL NOT NULL,
    captured_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, captured_at)
);

SELECT create_hypertable('market_snapshots', 'captured_at', if_not_exists => TRUE);
CREATE INDEX idx_snapshots_ticker_time ON market_snapshots(ticker, captured_at);
```

**007_calibration.sql** — Model accuracy tracking
```sql
CREATE TABLE calibration (
    id              BIGSERIAL,
    ticker          TEXT NOT NULL,
    signal_type     TEXT NOT NULL,
    model_prob      REAL NOT NULL,
    market_price    REAL NOT NULL,
    actual_outcome  BOOLEAN NOT NULL,     -- true = yes settled
    prob_bucket     TEXT NOT NULL,         -- '0.7-0.8', etc.
    sigma_used      REAL NOT NULL,
    settled_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, settled_at)
);

SELECT create_hypertable('calibration', 'settled_at', if_not_exists => TRUE);
CREATE INDEX idx_cal_type_bucket ON calibration(signal_type, prob_bucket);

-- Continuous aggregate for rolling calibration (TimescaleDB feature)
CREATE MATERIALIZED VIEW calibration_rolling
WITH (timescaledb.continuous) AS
SELECT
    signal_type,
    prob_bucket,
    time_bucket('1 day', settled_at) AS day,
    COUNT(*) AS total,
    SUM(CASE WHEN actual_outcome THEN 1 ELSE 0 END) AS wins,
    AVG(model_prob) AS avg_model_prob,
    AVG(CASE WHEN actual_outcome THEN 1.0 ELSE 0.0 END) AS actual_win_rate
FROM calibration
GROUP BY signal_type, prob_bucket, time_bucket('1 day', settled_at);
```

### Improvement over original plan
- **Hypertables** on time-series tables (signals, observations, market_snapshots, calibration)
- **Continuous aggregate** for calibration rolling stats — no manual queries needed
- **Idempotency key** on orders table — prevents duplicate orders on crash recovery
- **Orderbook depth fields** in market_snapshots — captures more than just yes/no price

### Verification
- `sqlx migrate run` succeeds
- `sqlx prepare` generates offline query data
- `\dt` shows all 7 tables + 1 continuous aggregate
- Insert test rows into each table, verify constraints hold

---

## BE-1.3: Configuration System

### Deliverable
`config/.env.example` and `rust/src/config.rs`.

### Environment Variables
```env
# Database
DATABASE_URL=postgres://tradebot:tradebot_dev@localhost:5432/tradebot

# Redis (KV cache only)
REDIS_URL=redis://localhost:6379

# NATS (messaging)
NATS_URL=nats://localhost:4222

# Kalshi
KALSHI_API_KEY=<your-key-id>
KALSHI_PRIVATE_KEY_PATH=<path-to-pem>
KALSHI_BASE_URL=https://demo-api.kalshi.co
KALSHI_WS_URL=wss://demo-api.kalshi.co/trade-api/ws/v2

# Binance
BINANCE_WS_URL=wss://stream.binance.com:9443/ws/btcusdt@trade

# Mesonet
MESONET_BASE_URL=https://mesonet.agron.iastate.edu

# Trading
PAPER_MODE=true
MAX_TRADE_SIZE_CENTS=2500
MAX_DAILY_LOSS_CENTS=10000
MAX_POSITIONS=5
MAX_EXPOSURE_CENTS=15000
KELLY_FRACTION_MULTIPLIER=0.25

# Logging
LOG_LEVEL=info
LOG_FORMAT=json

# Alerts (optional)
DISCORD_WEBHOOK_URL=

# Server
HTTP_PORT=3000
```

### Rust Config Struct
```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub nats_url: String,
    pub kalshi_api_key: String,
    pub kalshi_private_key_path: String,
    pub kalshi_base_url: String,
    pub kalshi_ws_url: String,
    pub binance_ws_url: String,
    pub mesonet_base_url: String,
    pub paper_mode: bool,
    pub max_trade_size_cents: i64,
    pub max_daily_loss_cents: i64,
    pub max_positions: usize,
    pub max_exposure_cents: i64,
    pub kelly_fraction_multiplier: f64,
    pub log_level: String,
    pub log_format: String,
    pub discord_webhook_url: Option<String>,
    pub http_port: u16,
}
```

Load with `dotenvy` + `envy` crate (deserializes env vars into struct via serde).

### Verification
- Binary starts and logs all non-secret config values
- Missing `KALSHI_API_KEY` causes immediate exit with clear error
- Missing optional fields (like `DISCORD_WEBHOOK_URL`) don't cause crash

---

## BE-1.4: Structured Logging Setup

### Deliverable
`tracing` + `tracing-subscriber` initialized in `main.rs`.

### Specification
- JSON output format (one JSON object per log line)
- Configurable log level via `LOG_LEVEL` env var
- Span context for request tracing (each order gets a span with `ticker`, `direction`)
- Fields: `timestamp`, `level`, `target`, `span`, `message`, `fields`

### Key crates
- `tracing` — spans and events
- `tracing-subscriber` — JSON formatter, env filter
- `tracing-appender` — optional file output

### Verification
- Starting the binary produces structured JSON log lines to stdout
- `LOG_LEVEL=debug` shows debug messages, `LOG_LEVEL=warn` suppresses info
- Order-related logs include `ticker`, `direction`, `size_cents`, `latency_ms` fields

---

## Acceptance Criteria (BE-1 Complete)

- [ ] `just db-up` starts PostgreSQL (TimescaleDB), Redis, NATS
- [ ] `just migrate` creates all 7 tables + continuous aggregate
- [ ] Rust binary loads config from `.env`, logs config at startup
- [ ] Structured JSON logging works with configurable level
- [ ] All services have health checks passing
