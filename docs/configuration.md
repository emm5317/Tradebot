# Configuration

All configuration is via environment variables. See `config/.env.example` for the full template.

## Environment Variables

### Database

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL (TimescaleDB) connection | required |
| `DATABASE_POOL_SIZE` | Connection pool size | `10` |

### Redis & NATS

| Variable | Description | Default |
|----------|-------------|---------|
| `REDIS_URL` | Redis for state cache | `redis://localhost:6379` |
| `NATS_URL` | NATS messaging | `nats://localhost:4222` |

### Kalshi

| Variable | Description | Default |
|----------|-------------|---------|
| `KALSHI_API_KEY` | Kalshi API key | required |
| `KALSHI_PRIVATE_KEY_PATH` | RSA private key for signing | required |
| `KALSHI_BASE_URL` | Kalshi REST API URL | `https://api.elections.kalshi.com` |
| `KALSHI_WS_URL` | Kalshi WebSocket URL | `wss://api.elections.kalshi.com/trade-api/ws/v2` |

### Crypto Feeds

| Variable | Description | Default |
|----------|-------------|---------|
| `ENABLE_COINBASE` | Coinbase feed | `true` |
| `ENABLE_BINANCE_SPOT` | Binance spot feed | `true` |
| `ENABLE_BINANCE_FUTURES` | Binance futures feed | `true` |
| `ENABLE_DERIBIT` | Deribit DVOL feed | `true` |
| `BINANCE_SPOT_WS_URL` | Binance US WebSocket URL | `wss://stream.binance.us:9443/ws/btcusd@trade` |
| `BINANCE_US_API_KEY` | Binance US API key | (optional) |
| `BINANCE_US_SECRET_KEY` | Binance US secret key | (optional) |

### RTI Venue Weighting (Rust Crypto)

| Variable | Description | Default |
|----------|-------------|---------|
| `RTI_STALE_THRESHOLD_SECS` | Venue staleness cutoff | `5` |
| `RTI_OUTLIER_THRESHOLD_PCT` | Outlier deviation cap | `0.5` |
| `RTI_MIN_VENUES` | Min healthy venues for reliable RTI | `2` |

### Trading

| Variable | Description | Default |
|----------|-------------|---------|
| `PAPER_MODE` | Paper trading (no real orders) | `true` |
| `MAX_TRADE_SIZE_CENTS` | Per-order limit | `25000` ($250) |
| `MAX_DAILY_LOSS_CENTS` | Daily stop-loss | `100000` ($1,000) |
| `MAX_POSITIONS` | Max concurrent positions | `25` |
| `MAX_EXPOSURE_CENTS` | Maximum total exposure | `1000000` ($10,000) |
| `KELLY_FRACTION_MULTIPLIER` | Kelly scaling factor | `0.25` |

### Crypto Evaluation

| Variable | Description | Default |
|----------|-------------|---------|
| `CRYPTO_ENTRY_MIN_MINUTES` | Min minutes to expiry for entry | `3.0` |
| `CRYPTO_ENTRY_MAX_MINUTES` | Max minutes to expiry for entry | `20.0` |
| `CRYPTO_MIN_EDGE` | Minimum edge to trade | `0.03` |
| `CRYPTO_MAX_EDGE` | Maximum edge (reject as miscalibration above this) | `0.25` |
| `CRYPTO_MIN_KELLY` | Minimum Kelly fraction | `0.02` |
| `CRYPTO_MIN_CONFIDENCE` | Minimum model confidence | `0.40` |
| `CRYPTO_COOLDOWN_SECS` | Per-ticker cooldown after trade | `30` |
| `WEATHER_COOLDOWN_SECS` | Per-ticker cooldown for weather | `120` |

### Multi-Asset Crypto

| Variable | Description | Default |
|----------|-------------|---------|
| `ENABLE_CRYPTO_BTC` | Enable BTC contract trading | `true` |
| `ENABLE_CRYPTO_ETH` | Enable ETH contract trading | `false` |
| `ENABLE_CRYPTO_SOL` | Enable SOL contract trading | `false` |
| `ENABLE_CRYPTO_XRP` | Enable XRP contract trading | `false` |
| `ENABLE_CRYPTO_DOGE` | Enable DOGE contract trading | `false` |

Each enabled asset gets its own CryptoState, Coinbase product subscription, and Binance spot stream. Binance futures and Deribit DVOL are BTC-only regardless of these flags. Per-asset feed health is tracked as `coinbase_{asset}` and `binance_spot_{asset}`.

### Kill Switches

| Variable | Description | Default |
|----------|-------------|---------|
| `KILL_SWITCH_ALL` | Disable all trading | `false` |
| `KILL_SWITCH_CRYPTO` | Disable crypto trading | `false` |
| `KILL_SWITCH_WEATHER` | Disable weather trading | `false` |

### Logging & Alerts

| Variable | Description | Default |
|----------|-------------|---------|
| `LOG_LEVEL` | Log level | `info` |
| `LOG_FORMAT` | Log format (json/text) | `json` |
| `DISCORD_WEBHOOK_URL` | Discord alert webhook | (optional) |

### Grafana

| Variable | Description | Default |
|----------|-------------|---------|
| `GRAFANA_ADMIN_PASSWORD` | Grafana admin password | `tradebot_grafana` |

Grafana runs on port 3033 with 4 auto-provisioned dashboards and 5 alert rules.

### Server

| Variable | Description | Default |
|----------|-------------|---------|
| `HTTP_PORT` | HTTP server port | `3030` |

### Weather Data Sources

| Variable | Description | Default |
|----------|-------------|---------|
| `MESONET_BASE_URL` | Iowa Mesonet API URL | `https://mesonet.agron.iastate.edu` |

## Notes

- **`PAPER_MODE=true`** is the default — the bot will not place real orders until explicitly switched
- All crypto feeds are enabled by default; disable individually if not needed
- RTI parameters control the dynamic venue weighting algorithm (see [trading models](trading-models.md))
- The production Kalshi API (`api.elections.kalshi.com`) is the default; use `demo-api.kalshi.co` only for integration testing with test accounts
- Trading limits are read from `.env` via docker-compose interpolation — change them in `.env` and restart the tradebot container
