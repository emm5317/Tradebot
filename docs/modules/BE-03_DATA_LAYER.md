# BE-3: Data Layer — Observations, Crypto Feed, Collection

**Dependencies**: BE-1 (database, config)
**Blocks**: BE-4 (signal engine), BE-8 (backtesting)
**Language**: Python

---

## Overview

The data layer fetches real-time observations (weather + crypto), stores them continuously, and pulls historical data for backtesting. This is a pure Python module — all data processing happens here before signals are evaluated.

---

## BE-3.1: ASOS Observation Fetcher

### Deliverable
`python/data/mesonet.py`

### Specification

```python
@dataclass
class ASOSObservation:
    station: str
    observed_at: datetime
    temperature_f: float | None
    wind_speed_kts: float | None
    wind_gust_kts: float | None
    precip_inch: float | None
    raw: dict
    staleness_seconds: float
    is_stale: bool  # True if > 300 seconds old

async def fetch_observation(station: str) -> ASOSObservation:
    """Fetch latest 1-minute ASOS observation from Iowa State Mesonet."""

async def fetch_all_stations(stations: list[str]) -> dict[str, ASOSObservation]:
    """Fetch for all configured stations concurrently."""
```

### Key package: `httpx`
- Async HTTP/2 client
- Built-in retry with `httpx.AsyncClient(transport=httpx.AsyncHTTPTransport(retries=3))`
- Connection pooling across calls

### Data source
Iowa State Mesonet JSON API:
```
https://mesonet.agron.iastate.edu/json/current.py?station={station}&network=ASOS
```

### Error handling
- Network errors: retry 3x with 2s backoff
- Missing data fields: return `None` for that field, don't fail
- Stale data: flag observations > 5 minutes old (signal evaluator will skip)

### Verification
- Fetch KORD (Chicago), KJFK (NYC), KDEN (Denver) — print formatted output
- Verify staleness check works (compare `observed_at` to current time)

---

## BE-3.2: Binance BTC WebSocket Feed

### Deliverable
`python/data/binance_ws.py`

### Specification

```python
class BinanceFeed:
    spot_price: float
    bars_1m: deque[OHLCBar]       # last 60 bars
    realized_vol_30m: float        # annualized from 1-min log returns

    async def connect(self):
        """Connect to Binance WS, maintain state."""

    def get_state(self) -> CryptoState:
        """Snapshot of current spot, vol, bar data."""
```

### Key package: `websockets`
- Purpose-built WebSocket library
- Handles ping/pong correctly
- Better backpressure handling than aiohttp WS

### Volatility calculation
```python
# Every minute, compute from last 30 1-min log returns
log_returns = [log(bar[i].close / bar[i-1].close) for i in range(1, 31)]
sigma_1min = std(log_returns)
sigma_annual = sigma_1min * sqrt(525600)  # minutes in a year
```

### Auto-reconnect
- Exponential backoff: 1s, 2s, 4s, 8s, max 30s
- On reconnect, bars buffer preserved (only gap in real-time price)
- Log every disconnect/reconnect

### Verification
- Run for 5 minutes, print spot + vol every 10 seconds
- Verify vol in reasonable range (annualized 30-80% for BTC)
- Kill connection, verify reconnect within 30s

---

## BE-3.3: Continuous Data Collector Daemon

### Deliverable
`python/collector/daemon.py` — standalone process, always running.

### Specification

```python
class CollectorDaemon:
    """Always-on process that builds historical dataset passively."""

    async def run(self):
        await asyncio.gather(
            self.collect_asos_loop(),
            self.collect_market_snapshots_loop(),
            self.collect_btc_loop(),
        )

    async def collect_asos_loop(self):
        """Every 60s: fetch all station observations → insert into observations table."""

    async def collect_market_snapshots_loop(self):
        """Every 60s: for contracts within 30 min of settlement, snapshot prices."""

    async def collect_btc_loop(self):
        """Every 60s: write BTC spot + vol to observations table."""
```

### Key packages
- `asyncpg` — fastest PostgreSQL driver for Python (C-extension backed)
- `httpx` — for Kalshi REST snapshots
- `pydantic` — validate data before insert

### Database writes
- Batch inserts using `asyncpg.copy_records_to_table()` for bulk efficiency
- Each collection loop is independent — one failing doesn't stop the others
- Structured logging with `structlog`

### Improvement over original plan
- **`asyncpg`** instead of `psycopg2` — 3-5x faster for inserts
- **Batch inserts** — reduces DB round-trips
- **Independent loops** — fault isolation between ASOS, Kalshi, and Binance collection
- **`pydantic` validation** — catches bad API responses before they enter the DB

### Verification
- Run for 1 hour
- Query `observations` — at least 50 ASOS rows per station
- Query `market_snapshots` — at least 1 row per minute per near-settlement contract
- Verify no gaps in collection (check timestamps)

---

## BE-3.4: Kalshi Historical Data Pull

### Deliverable
`python/data/kalshi_history.py`

### Specification

```python
async def pull_settlement_history(months: int = 12) -> int:
    """Pull all settled weather + crypto contracts. Returns count."""

async def pull_historical_prices(ticker: str) -> list[MarketSnapshot]:
    """Pull historical price snapshots for a specific contract."""
```

- Handles Kalshi pagination (max 200 per page, cursor-based)
- Idempotent: upsert by ticker (safe to re-run)
- Respects rate limits (100 req/min — pace requests accordingly)

### Verification
- Pull 12 months of weather settlements
- Count by city — at least 50 per major city
- Verify `settled_yes` and `close_price` populated

---

## Acceptance Criteria (BE-3 Complete)

- [ ] ASOS fetcher returns valid observations for 3+ stations
- [ ] Binance feed maintains spot price + 30-min vol
- [ ] Collector daemon stores data continuously without gaps
- [ ] Historical pull ingests 12 months of Kalshi settlements
- [ ] All data validated with pydantic before database insert
