# Configuration

All configuration is via environment variables. See `config/.env.example` for the full template.

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL (TimescaleDB) connection | required |
| `REDIS_URL` | Redis for state cache | `redis://localhost:6379` |
| `NATS_URL` | NATS messaging | `nats://localhost:4222` |
| `KALSHI_API_KEY` | Kalshi API key | required |
| `KALSHI_PRIVATE_KEY_PATH` | RSA private key for signing | required |
| `PAPER_MODE` | Paper trading (no real orders) | `true` |
| `MAX_TRADE_SIZE_CENTS` | Per-order limit | `2500` ($25) |
| `MAX_DAILY_LOSS_CENTS` | Daily stop-loss | `10000` ($100) |
| `MAX_POSITIONS` | Max concurrent positions | `5` |
| `KELLY_FRACTION_MULTIPLIER` | Kelly scaling factor | `0.25` |
| `ENABLE_COINBASE` | Coinbase feed | `false` |
| `ENABLE_BINANCE_SPOT` | Binance spot feed | `false` |
| `ENABLE_BINANCE_FUTURES` | Binance futures feed | `false` |
| `ENABLE_DERIBIT` | Deribit DVOL feed | `false` |
| `RTI_STALE_THRESHOLD_SECS` | Venue staleness cutoff | `5` |
| `RTI_OUTLIER_THRESHOLD_PCT` | Outlier deviation cap | `0.5` |
| `RTI_MIN_VENUES` | Min healthy venues for reliable RTI | `2` |
| `DISCORD_WEBHOOK_URL` | Alert notifications | (optional) |

## Notes

- **`PAPER_MODE=true`** is the default — the bot will not place real orders until explicitly switched
- Crypto feeds are disabled by default; enable individually as needed
- RTI parameters control the dynamic venue weighting algorithm (see [trading models](trading-models.md))
- The Kalshi demo environment is recommended for initial testing before connecting to production
