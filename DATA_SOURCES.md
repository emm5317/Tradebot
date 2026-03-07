# DATA_SOURCES.md
External data sources, endpoints, and integration details for tradebot.
---
## Weather: ASOS Observations
ASOS (Automated Surface Observing System) stations are the ground truth for Kalshi weather contract settlements.
### Primary: Iowa State Mesonet (1-Minute Data)
**Use this for live trading.** It provides the freshest ASOS observations available anywhere — 1-minute resolution, free, no auth.
```
Base URL: https://mesonet.agron.iastate.edu/request/asos/1min.php
Parameters:
  station  — ICAO code (e.g., KORD, KJFK)
  sts      — start time (YYYY-mm-dd HH:MM)
  ets      — end time
  vars     — comma-separated: tmpf, dwpf, sknt, gust, vsby, p01i
Example (last 2 hours for Chicago O'Hare):
  https://mesonet.agron.iastate.edu/request/asos/1min.php?station=KORD&sts=2024-07-15+17:00&ets=2024-07-15+19:00&vars=tmpf
```
**Key fields:**
| Field | Description | Unit | Contract type |
|-------|-------------|------|---------------|
| `tmpf` | Temperature | °F | High/Low temp contracts |
| `dwpf` | Dew point | °F | — |
| `sknt` | Wind speed | knots | Wind contracts |
| `gust` | Peak gust | knots | Wind gust contracts |
| `p01i` | 1-hour precip | inches | Rain contracts |
| `vsby` | Visibility | miles | — |
**Freshness**: Data arrives within 1-3 minutes of observation. For near-expiry trading, this is the best available source.
**Historical data**: Same endpoint supports arbitrary date ranges. Use this for backtesting.
### Fallback: Aviation Weather METAR
Use only if Mesonet is down. Updates every 20-60 minutes — too stale for near-expiry trading.
```
URL: https://aviationweather.gov/api/data/metar?ids=KORD&format=json&hours=2
Auth: None
Rate limit: Reasonable use (no documented limit)
```
### Target Stations
| City | ICAO | Kalshi contracts | Notes |
|------|------|-----------------|-------|
| Chicago | KORD | Temp, wind | High volume |
| New York (JFK) | KJFK | Temp, wind | High volume |
| New York (LGA) | KLGA | Temp | Alternative station |
| Los Angeles | KLAX | Temp | Lower temp variance |
| Miami | KMIA | Temp, rain | Hurricane season |
| Denver | KDEN | Temp, wind | High variance |
| Dallas | KDFW | Temp | Summer extremes |
| Atlanta | KATL | Temp | — |
| Omaha | KOMA | Temp | Smaller market (less competition) |
| Memphis | KMEM | Temp | Smaller market |
**Hypothesis to test**: Smaller markets (Omaha, Memphis) may have less sophisticated market makers, producing wider edges. Backtest both tiers.
---
## Crypto: Binance BTC Spot
### WebSocket Stream (Live Trading)
```
URL: wss://stream.binance.com:9443/ws/btcusdt@trade
Auth: None (public endpoint)
Format: JSON per trade
Message:
{
  "e": "trade",
  "s": "BTCUSDT",
  "p": "67234.50",    ← price (string)
  "q": "0.00123",     ← quantity
  "T": 1721069280000  ← trade time (ms)
}
```
**Latency**: Sub-100ms from trade execution to WebSocket delivery.
**Rolling volatility calculation**: Accumulate 1-minute OHLC bars from the tick stream. Compute realized vol from the last 30 one-minute returns, annualized:
```python
returns = np.diff(np.log(close_prices[-31:]))  # 30 returns from 31 prices
sigma_1min = np.std(returns)
sigma_annual = sigma_1min * np.sqrt(525600)     # minutes in a year
```
### REST API (Historical / Backtest)
```
URL: https://api.binance.com/api/v3/klines
Parameters:
  symbol    — BTCUSDT
  interval  — 1m
  startTime — epoch ms
  endTime   — epoch ms
  limit     — 1000 (max)
Auth: None
Rate limit: 1200 requests/min (generous)
```
---
## Kalshi Trading API v2
### Authentication
Kalshi uses RSA-SHA256 signed requests. The private key is generated on the Kalshi dashboard.
```
Signing format:
  timestamp + method + path

Example:
  "1721069280POST/trade-api/v2/portfolio/orders"

Sign with RSA-SHA256, Base64-encode, send as KALSHI-ACCESS-SIGNATURE header.
```
Required headers on every request:
- `KALSHI-ACCESS-KEY` — your API key ID
- `KALSHI-ACCESS-SIGNATURE` — RSA-SHA256 signature (Base64)
- `KALSHI-ACCESS-TIMESTAMP` — Unix epoch seconds (string)
### REST Endpoints
| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/trade-api/v2/markets` | GET | List active markets (paginated) |
| `/trade-api/v2/markets/{ticker}` | GET | Single market details |
| `/trade-api/v2/portfolio/orders` | POST | Place order |
| `/trade-api/v2/portfolio/orders` | GET | List your orders |
| `/trade-api/v2/portfolio/positions` | GET | Current positions |
| `/trade-api/v2/portfolio/settlements` | GET | Settlement history |
Base URL: `https://trading-api.kalshi.com`
Demo URL: `https://demo-api.kalshi.co` (paper trading)
### WebSocket Feed
```
URL: wss://trading-api.kalshi.com/trade-api/ws/v2
Auth: Same RSA-SHA256 signing on the upgrade request
Subscribe message:
{
  "id": 1,
  "cmd": "subscribe",
  "params": {
    "channels": ["orderbook_delta"],
    "market_tickers": ["KXTEMP-24-HI-T68-20240715"]
  }
}
Response: orderbook_delta messages with yes/no price + size changes
```
**Critical**: Use the WebSocket feed for market prices, not REST polling. REST polling adds 500ms+ latency per cycle. WebSocket delivers updates in ~10ms.
### Order Placement
```json
POST /trade-api/v2/portfolio/orders
{
  "ticker": "KXTEMP-24-HI-T68-20240715",
  "action": "buy",
  "side": "yes",
  "type": "market",
  "count": 5
}
```
- `count` is number of contracts (each contract is $0.01 to $0.99)
- Market orders fill immediately at best available price
- Response includes `order_id`, `status`, `avg_fill_price`
### Rate Limits
Kalshi documents a general rate limit but does not publish exact numbers. In practice, the system should not exceed 10 orders per minute — our strategy is low-frequency by design (a few trades per day).
---
## Redis Streams
Internal bridge between Python signal engine and Rust execution engine.
```
Stream key: "tradebot:signals"
Message format (JSON string in a single field):
{
  "ticker": "KXTEMP-24-HI-T68-20240715",
  "direction": "yes",
  "edge": 0.08,
  "kelly_fraction": 0.062,
  "model_prob": 0.85,
  "market_price": 0.77,
  "category": "weather",
  "minutes_to_settlement": 12.5,
  "timestamp": "2024-07-15T19:48:00Z"
}
Python publisher:
  redis.xadd("tradebot:signals", {"data": json.dumps(signal)})
Rust consumer:
  XREAD BLOCK 1000 STREAMS tradebot:signals $
```
Consumer group: `tradebot-execution`. Single consumer: `engine-1`. This ensures exactly-once processing even across restarts (unacknowledged messages are re-delivered).
---
## PostgreSQL 17
```
Host: localhost (Docker) or configured via DATABASE_URL
Port: 5432
Database: tradebot
Schema: managed by sqlx-cli migrations
Connection string:
  postgres://tradebot:password@localhost:5432/tradebot
```
See `migrations/` for table definitions. Key tables: `contracts`, `signals`, `orders`, `daily_summary`.
---
## Macro Event Calendar
For crypto signal blackout. Source: manually maintained JSON or pulled from a financial calendar API.
```json
// config/blackout_events.json
{
  "events": [
    {"type": "FOMC", "datetime": "2024-07-31T18:00:00Z"},
    {"type": "CPI", "datetime": "2024-08-14T12:30:00Z"},
    {"type": "NFP", "datetime": "2024-08-02T12:30:00Z"}
  ]
}
```
Check logic: if any event in the list is within 30 minutes of the current time, suppress all crypto signals. Weather signals are unaffected.
Future improvement: pull events automatically from a calendar API (e.g., Trading Economics, Forex Factory RSS). For now, manual JSON is reliable enough for the 6-8 events per month that matter.
