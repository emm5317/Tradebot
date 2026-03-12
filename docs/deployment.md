# Deployment & Operations Guide

## Architecture

Tradebot runs as a set of Docker containers orchestrated by Docker Compose:

```
┌─────────────────────────────────────────────────────────────┐
│  Docker Compose Stack                                       │
│                                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────┐  │
│  │ postgres │  │  redis   │  │   nats   │  │  grafana  │  │
│  │ :15432   │  │  :6379   │  │  :4222   │  │  :3033    │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └─────┬─────┘  │
│       │              │              │               │       │
│  ┌────┴──────────────┴──────────────┴───────────────┘       │
│  │                                                          │
│  ├── tradebot        (Rust binary, :3030)                   │
│  ├── collector       (Python data collection)               │
│  ├── evaluator       (Python weather signals)               │
│  ├── calibrator      (Python calibration agent)             │
│  ├── contract-sync   (Python contract sync, 5min loop)      │
│  └── migrate         (SQL migrations, runs once)            │
│                                                             │
└─────────────────────────────────────────────────────────────┘

External:
  └── dashboard        (Python FastAPI, :8050, runs locally)
```

## Quick Start

```bash
# Start everything
just db-up          # Postgres, Redis, NATS + migrations
just up             # All containers (tradebot, collector, evaluator, etc.)
just grafana        # Grafana dashboards on :3033
just dashboard      # Bloomberg terminal dashboard on :8050 (local)
```

## Container Reference

| Container | Image | Purpose | Ports |
|-----------|-------|---------|-------|
| `postgres` | `timescale/timescaledb:latest-pg17` | Database (TimescaleDB) | 15432→5432 |
| `redis` | `redis:7-alpine` | State cache | 6379 |
| `nats` | `nats:2-alpine` | Messaging (JetStream) | 4222, 8222 |
| `tradebot` | Custom (Rust+Python) | Feeds, crypto eval, execution | 3030 |
| `collector` | Custom (Python) | Weather + market data collection | — |
| `evaluator` | Custom (Python) | Weather signal generation (10s) | — |
| `calibrator` | Custom (Python) | Calibration agent (hourly) | — |
| `contract-sync` | Custom (Python) | Kalshi contract sync (5min) | — |
| `grafana` | `grafana/grafana-oss:11.4.0` | Observability dashboards | 3033 |
| `migrate` | `timescale/timescaledb:latest-pg17` | SQL migrations (exit after) | — |

## Configuration

All configuration is via environment variables in `.env`. See [configuration.md](configuration.md) for the full reference.

Key settings are interpolated into Docker Compose via `${VAR:-default}` syntax. To change trading limits:

1. Edit `.env`
2. Restart the affected container: `docker compose -f docker/docker-compose.yml --env-file .env up -d --force-recreate tradebot`

### Current Trading Limits (Paper Mode)

| Setting | Value |
|---------|-------|
| Paper mode | `true` |
| Max trade size | $250 (25000 cents) |
| Max daily loss | $1,000 (100000 cents) |
| Max positions | 25 |
| Max exposure | $10,000 (1000000 cents) |
| Kelly fraction | 0.25 |

## Rebuilding

When code changes, containers must be rebuilt:

```bash
# Rebuild a specific service
docker compose -f docker/docker-compose.yml --env-file .env build tradebot
docker compose -f docker/docker-compose.yml --env-file .env up -d tradebot

# Rebuild all Python services (they share the same Dockerfile)
docker compose -f docker/docker-compose.yml --env-file .env build collector evaluator calibrator contract-sync
docker compose -f docker/docker-compose.yml --env-file .env up -d collector evaluator calibrator contract-sync

# Shortcut: rebuild + restart everything
docker compose -f docker/docker-compose.yml --env-file .env up -d --build
```

## Credentials

| File | Contains | Gitignored |
|------|----------|------------|
| `.env` | API keys, DB passwords, trading config | Yes (`*.env` in `.gitignore`) |
| `config/kalshi_prod.pem` | Kalshi RSA private key | Yes (`*.pem` in `.gitignore`) |

**Never commit credentials.** Both files are in `.gitignore`. The PEM is baked into the Docker image at build time via `COPY config/ config/`.

## Monitoring

### Grafana (port 3033)

4 auto-provisioned dashboards:
- **Trading Overview** — P&L, signal rate, fill rate, positions
- **Feed Health** — Per-feed staleness, score trends, outage detection
- **System Health** — DB connections, NATS queue depth, Redis ops
- **Decision Audit** — Rejection breakdown, edge distribution, eval latency

5 alert rules (Discord webhook):
- No signals for 2 hours
- Daily loss exceeds $50
- Feed stale for >5 minutes
- Rejection rate >90%
- Calibrator stale (no run in 2 hours)

### Dashboard (port 8050)

Bloomberg terminal-style UI with 6 pages:
- **MAIN** — Active contracts, positions summary, signal heatmap
- **SGNL** — Signal log with filters, rejection breakdown
- **EXEC** — Fill rate, latency histogram, order log
- **ANAL** — Brier trend, calibration curve, P&L charts
- **RISK** — Exposure bars, kill switches, feed health matrix
- **WEAT** — Station cards, HRRR skill heatmap, settlement outcomes

### Health Endpoints (Rust binary, port 3030)

| Endpoint | Purpose |
|----------|---------|
| `GET /health/live` | Liveness probe — always returns 200 if process is running |
| `GET /health/ready` | Readiness probe — checks DB, Redis, NATS, feed connectivity |
| `GET /metrics` | Prometheus metrics (eval latency, order counts, feed health scores) |

### Health Check

```bash
just health          # curl localhost:8050/api/health
curl localhost:3030/health/ready   # Readiness probe
curl localhost:3030/metrics        # Prometheus metrics
just ps              # Docker container status
just logs            # Follow tradebot logs
just logs-all        # Follow all container logs
```

## Database

### Migrations

23 SQL migrations (000-022) in `migrations/`. All use `IF NOT EXISTS` / `ON CONFLICT` — safe to re-run. Migrations run automatically on container startup via the `migrate` service.

```bash
just migrate         # Run all migrations
just db-shell        # Open psql shell
just db-reset        # WARNING: Destroys all data, recreates from scratch
```

### Key Tables

| Table | Purpose |
|-------|---------|
| `contracts` | Kalshi contract metadata + settlement outcomes |
| `signals` | All generated signals (acted on + rejected) |
| `orders` | Order lifecycle with 10-state tracking |
| `observations` | ASOS weather observations (hypertable) |
| `metar_observations` | METAR data with 6-hr max/min groups |
| `hrrr_forecasts` | HRRR 15-min forecast temperatures |
| `crypto_ticks` | Multi-exchange crypto price ticks |
| `market_snapshots` | Kalshi orderbook snapshots (hypertable) |
| `decision_log` | Every eval outcome (accepted/rejected + reason) |
| `feed_health_log` | 60-second feed health snapshots |
| `strategy_performance` | Daily per-strategy P&L and Brier scores |
| `station_calibration` | Per-(station, month, hour) model parameters |
| `calibration_metrics` | Rolling calibration accuracy metrics |
| `backtest_runs` | Parameter sweep results |

## Troubleshooting

### Kalshi WS 401 Unauthorized
- Check `KALSHI_API_KEY` in `.env` matches your Kalshi account
- Verify `config/kalshi_prod.pem` contains the correct RSA private key
- Rebuild the tradebot image (`docker compose build tradebot`) since the PEM is baked in

### DNS resolution errors in containers
- Restart the affected container: `docker compose restart evaluator`
- If persistent, restart Docker Desktop

### Postgres connection timeout
- Ensure the postgres container is healthy: `docker compose ps postgres`
- Local Rust binary can't connect to Docker postgres at `localhost:15432` — use the Dockerized tradebot instead

### Stale orderbook warnings
- Expected for low-liquidity Kalshi contracts (thin books)
- If all tickers are stale, check Kalshi WS connection in tradebot logs

### Feed health alerts
- Binance US has naturally lower activity — 10s staleness threshold is normal
- Coinbase/Binance futures should update within 5s
- Deribit DVOL updates less frequently (index calculation)
