# Configuration

All configuration is via environment variables. See `config/.env.example` for the full template.

## Environment Variables

### Database

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL (TimescaleDB) connection | required |
| `DATABASE_POOL_SIZE` | Connection pool size | `20` |

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
| `KALSHI_BASE_URL` | Kalshi REST API URL | `https://demo-api.kalshi.co` |
| `KALSHI_WS_URL` | Kalshi WebSocket URL | `wss://demo-api.kalshi.co/trade-api/ws/v2` |

### Crypto Feeds

| Variable | Description | Default |
|----------|-------------|---------|
| `ENABLE_COINBASE` | Coinbase feed | `false` |
| `ENABLE_BINANCE_SPOT` | Binance spot feed | `false` |
| `ENABLE_BINANCE_FUTURES` | Binance futures feed | `false` |
| `ENABLE_DERIBIT` | Deribit DVOL feed | `false` |
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
| `MAX_TRADE_SIZE_CENTS` | Per-order limit | `2500` ($25) |
| `MAX_DAILY_LOSS_CENTS` | Daily stop-loss | `10000` ($100) |
| `MAX_POSITIONS` | Max concurrent positions | `5` |
| `MAX_EXPOSURE_CENTS` | Maximum total exposure | `15000` ($150) |
| `KELLY_FRACTION_MULTIPLIER` | Kelly scaling factor | `0.25` |

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
- Crypto feeds are disabled by default; enable individually as needed
- RTI parameters control the dynamic venue weighting algorithm (see [trading models](trading-models.md))
- The Kalshi demo environment (`demo-api.kalshi.co`) is recommended for initial testing before connecting to production (`api.elections.kalshi.com`)
