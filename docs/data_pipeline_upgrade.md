# Data Pipeline Upgrade: Settlement-Focused Fair-Value Architecture

## Context

The Tradebot is a Rust + Python trading bot for Kalshi prediction markets (BTC crypto contracts and weather temperature contracts). The current system works but has fundamental gaps:

- **Crypto contracts** settle to the CFB RTI (60-second average from constituent exchanges like Coinbase, Bitstamp, Kraken) — but the bot only uses Binance spot price
- **Weather contracts** settle to the NWS Daily Climate Report using local standard time (not DST) — but the bot has no DST handling, no CLI/DSM parsing, no rounding ambiguity logic, and no HRRR forecast integration
- Contract metadata is extracted via regex from English titles instead of structured ticker parsing
- No historical replay with source attribution to prove each data source adds marginal value

The upgrade restructures around 4 pillars: (1) contract rules/settlement correctness, (2) Kalshi-first market state, (3) explicit fair-value engines per contract type, (4) historical replay and source attribution.

---

## Phase 1: Contract Rules Resolver (foundational — unblocks everything)

**Why first:** Eliminates fragile regex-from-title pattern. Both fair-value engines need exact settlement-source mapping.

### Step 1a: Ticker format discovery
Before building the parser, catalog all series ticker formats from existing Kalshi API data. Run a one-time script that:
1. Queries all tickers from `contracts` table and Kalshi API `/markets` endpoint
2. Groups by series prefix pattern (e.g., `KXBTCD`, `KXTEMP`, etc.)
3. For each series: extract the encoding pattern for date, strike, station/city, max vs min
4. Outputs a mapping file that becomes the basis for `SERIES_CONFIG`

This discovery step avoids hardcoding assumptions about ticker formats.

### New files
- `migrations/009_contract_rules.sql` — `contract_rules` table with series_ticker, market_ticker, contract_type (`crypto_binary`/`weather_max`/`weather_min`), settlement_source (`cfb_rti`/`nws_cli_dsm`), settlement_station, settlement_tz (IANA), strike, expiry_time, day boundaries, constituent_exchanges
- `python/rules/__init__.py`
- `python/rules/resolver.py` — `ContractRulesResolver` class: loads rules from DB, caches by market_ticker, periodic refresh
- `python/rules/ticker_parser.py` — Parses structured ticker format (`KXBTCD-26MAR08-T98500`) instead of English titles. Maps series prefixes to known configs via `SERIES_CONFIG` dict (populated from discovery step). Replaces `_extract_threshold`/`_extract_station`/`_extract_city` in `kalshi_history.py`
- `python/rules/timezone.py` — `compute_day_boundaries(station_tz, date)`: returns local-standard-time midnight boundaries, ignoring DST. Uses `zoneinfo` stdlib
- `python/rules/discover.py` — One-time discovery script: catalogs all series ticker formats from API data, outputs `SERIES_CONFIG` mapping

### Modified files
- `python/signals/types.py` — Add `rules: ContractRules | None` to `Contract` model
- `python/evaluator/daemon.py` — Replace `_infer_signal_type` (line 216) with rules resolver lookup
- `python/data/kalshi_history.py` — `_upsert_contracts` (line 160) also upserts into `contract_rules` via ticker parser

### Key data structure
```python
@dataclass(frozen=True)
class ContractRules:
    market_ticker: str
    series_ticker: str
    contract_type: Literal['crypto_binary', 'weather_max', 'weather_min']
    settlement_source: str           # 'cfb_rti' or 'nws_cli_dsm'
    settlement_station: str | None   # ASOS code for weather
    settlement_tz: str | None        # IANA timezone
    strike: float
    expiry_time: datetime
    day_boundary_start: datetime | None  # weather: local-standard-time day start
    day_boundary_end: datetime | None
    constituent_exchanges: list[str]     # crypto: CFB RTI constituents
```

### Tests
- Unit: ticker parser maps known tickers correctly
- Unit: DST-ignoring day boundaries for all 5 stations, including DST transition edges
- Integration: load contract from DB → resolver returns correct rules

---

## Phase 2: Kalshi Market State Engine (Rust — can parallel with Phase 1)

**Why:** The bot already has WS orderbook_delta + trade channels. This phase adds the `ticker` channel, trade tape metrics, and richer Redis state.

### New files
- `rust/src/kalshi/trade_tape.rs` — `TradeTape` (bounded `VecDeque<TradeRecord>`) with `aggressiveness(window)`, `recent_volume(window)`, `vwap(window)`

### Modified files
- `rust/src/kalshi/websocket.rs` — Add `"ticker"` to channels list (line 173). Add `TickerUpdate` variant to `WsFeedMessage` enum (yes_bid, yes_ask, sizes, last_price, volume, open_interest, market_status). Add `"ticker"` arm to `handle_text_message` (line 242)
- `rust/src/kalshi/mod.rs` — Add `pub mod trade_tape`
- `rust/src/orderbook_feed.rs` — Enhanced Redis flush: add best_bid_size, best_ask_size, last_trade_price, trade_aggr_30s, recent_volume_60s, market_status, updated_at. Add market-state change handling (clear book on closed/settled, publish to NATS `market.kalshi.status.{ticker}`). Add periodic stale-feed monitoring (every 5s)
- `python/signals/types.py` — Add fields to `OrderbookState`: best_bid_size, best_ask_size, last_trade_price, last_trade_count, trade_aggr_30s, recent_volume_60s, market_status

### Tests
- Rust: trade tape aggressiveness, VWAP, windowed volume
- Rust: mock WS messages with ticker channel → verify Redis JSON output
- Python: verify evaluator parses new Redis fields, falls back gracefully

---

## Phase 3: Weather Fair-Value Engine (depends on Phase 1)

**Why:** Current Gaussian diffusion ensemble doesn't model settlement mechanics. Weather edge comes from understanding what number will appear in the CLI, not from better forecasting.

### New data sources
- `python/data/aviationweather.py` — REST fetch from `aviationweather.gov/api/data/metar`. Key: parse 6-hourly max/min groups (1xxxx/2xxxx METAR remarks) — these feed into the Daily Climate Report. Returns `METARObservation` with `max_temp_6hr`, `min_temp_6hr`
- `python/data/open_meteo.py` — REST fetch HRRR forecasts from `api.open-meteo.com`. 15-min resolution, updated hourly. Provides "forecast excursion" probability for remaining-day max/min estimation

### New model files
- `python/models/weather_fv.py` — Settlement-aware weather fair-value engine:
  1. Load rules (station, tz, max/min, strike)
  2. Compute local-standard-time day boundaries
  3. Track running max/min from all observations during the day
  4. If running max/min already exceeds strike → probability locked at ~1.0
  5. If not locked → blend observation trend (existing Gaussian diffusion) with HRRR forecast excursion probability
  6. Handle rounding ambiguity near threshold boundaries
  - Key types: `WeatherState` (running max/min, locked status, rounding flags) and `WeatherFairValue` (probability, confidence, already_locked, uncertainty_band)
- `python/models/rounding.py` — `compute_rounding_uncertainty(metar_temp_c, threshold_f)` → `(min_f, max_f, is_ambiguous)`. Handles METAR Celsius → CLI Fahrenheit conversion ambiguity

### New migration
- `migrations/010_weather_sources.sql` — `metar_observations` hypertable (with max/min 6hr fields) + `hrrr_forecasts` hypertable

### Modified files
- `python/signals/weather.py` — `evaluate()` uses `WeatherFairValue` engine instead of raw `compute_ensemble_probability`. Passes contract rules, loads/updates `WeatherState` per contract day
- `python/collector/daemon.py` — Add `collect_aviationweather_loop()` (60s) and `collect_hrrr_loop()` (300s)

### Tests
- Unit: rounding ambiguity known C/F edge cases
- Unit: "already locked" logic for max and min contracts
- Unit: day boundaries with DST
- Integration: replay known settlement day → verify probability converges correctly
- Backtest: compare new model Brier score vs current ensemble on historical contracts

---

## Phase 4: Crypto Fair-Value Engine (depends on Phase 1, can parallel with Phase 3)

**Why:** Bot uses Binance spot + realized vol in Black-Scholes, but Kalshi settles to CFB RTI (weighted mid from Coinbase, Bitstamp, Kraken, etc). Shadow RTI estimation from available constituents is the core improvement.

### Crypto feeds in Rust (latency-sensitive, future-proofed for event-driven signals)

Coinbase and Binance futures WebSocket feeds go in Rust, not Python. Rationale: these are high-frequency sub-second streams that benefit from Rust's async performance, and placing them in Rust enables a future move to reactive/event-driven signal generation without re-implementing the feeds. Data is published to Redis (same pattern as Kalshi orderbook) for Python model consumption.

### New Rust files
- `rust/src/feeds/mod.rs` — Module for external exchange feeds
- `rust/src/feeds/coinbase.rs` — `CoinbaseFeed`: persistent WS connection to `wss://advanced-trade-ws.coinbase.com`, subscribes to `level2` channel for BTC-USD (public, no auth). Maintains in-memory best bid/ask/mid. Flushes to Redis key `crypto:coinbase` every 500ms (same pattern as `orderbook_feed.rs`). Publishes to NATS `data.crypto.coinbase.spot`
- `rust/src/feeds/binance_futures.rs` — `BinanceFuturesFeed`: persistent WS to `wss://fstream.binance.com/stream?streams=btcusdt@aggTrade/btcusdt@depth@100ms/btcusdt@markPrice@1s`. Maintains perp_price, mark_price, funding_rate, order book imbalance. Flushes to Redis key `crypto:binance_futures` every 500ms. Publishes to NATS `data.crypto.binance.futures`
- `rust/src/feeds/deribit.rs` — `DeribitFeed` (optional, gated by config flag): persistent WS to `wss://www.deribit.com/ws/api/v2`, subscribes to `deribit_volatility_index.btc_usd`. Flushes DVOL to Redis key `crypto:deribit_dvol`. No auth required for public data

### Rust Redis key structure for crypto feeds
```
crypto:coinbase         # {spot, best_bid, best_ask, updated_at}
crypto:binance_futures  # {perp_price, mark_price, funding_rate, basis, obi, updated_at}
crypto:deribit_dvol     # {dvol, updated_at}
```

### Modified Rust files
- `rust/src/main.rs` — Spawn Coinbase, Binance futures, and (optionally) Deribit feed tasks alongside existing Kalshi WS and orderbook feed
- `rust/src/config.rs` — Add `coinbase_ws_url`, `binance_futures_ws_url`, `deribit_ws_url`, `enable_deribit`, `enable_coinbase`, `enable_binance_futures`
- `Cargo.toml` — No new crates needed; already has `tokio-tungstenite`, `fred`, `async-nats`, `serde_json`

### New Python model file
- `python/models/crypto_fv.py` — Multi-input crypto fair-value engine:
  - Reads Coinbase/Binance futures/Deribit data from Redis (same as it reads Kalshi orderbook)
  - Shadow RTI: weighted average of Binance spot + Coinbase spot (not exact replication, but estimation with measured error at expiry)
  - Inputs: `CryptoInputs` (binance_spot, coinbase_spot, perp_price, funding_rate, deribit_dvol, kalshi_book, contract_rules, minutes_remaining)
  - Output: `CryptoFairValue` (probability, confidence, shadow_rti, basis, component contributions)
  - Models 60-second RTI averaging window as moving average with vol scaling
  - Key types: `CryptoInputs`, `CryptoFairValue`

### New migration
- `migrations/011_crypto_sources.sql` — `crypto_ticks` hypertable (source, symbol, price, bid, ask, funding_rate, dvol, observed_at)

### Modified Python files
- `python/signals/crypto.py` — Accept `CryptoInputs` instead of scalar spot/vol. Use `CryptoFairValue` engine. Read Coinbase/Binance futures state from Redis
- `python/evaluator/daemon.py` — Fetch crypto feed states from Redis, construct `CryptoInputs`, pass to evaluator
- `python/config.py` — Add `enable_deribit`, feed URLs (mirroring Rust config for consistency)

### Tests
- Rust: unit tests for Coinbase/Binance futures WS message parsing, Redis flush format
- Rust: mock WS server → verify feed connects, parses, and writes to Redis correctly
- Python: unit test shadow RTI estimation against known values
- Python: unit test basis/funding rate signal extraction
- Backtest: new model vs old N(d2) on historical crypto contracts
- Integration: verify end-to-end flow: Rust feeds → Redis → Python model → signal

---

## Phase 5: Historical Capture & Replay (depends on Phases 2-4)

**Why:** Cannot prove a data source adds edge without replay and attribution testing. "Every new source must beat a 'does this improve out-of-sample calibration or PnL?' threshold."

### New migration
- `migrations/012_replay_tables.sql` — `kalshi_book_events` hypertable (raw WS events for orderbook replay) + `model_evaluations` hypertable (full model output + input snapshots at each evaluation)

### New file
- `python/backtester/replay.py` — `ReplayEngine` with source ablation: run backtest with different source subsets, compare Brier score / PnL deltas to attribute marginal lift per source

### Modified files
- `rust/src/orderbook_feed.rs` — Background writer: persist raw `WsFeedMessage` events to `kalshi_book_events` via batched inserts (flush every 1s or 100 events)
- `python/signals/publisher.py` — Add `publish_model_evaluation()`: persist full model output + input snapshot to `model_evaluations`

### Tests
- Verify raw event capture: insert mock events, replay through OrderbookManager, verify book state matches
- Source ablation: run same period with/without Coinbase, verify PnL/Brier delta measurement

---

## Phase 6: Gated Source Additions (ongoing, only after Phase 5 attribution)

Each new source follows the pattern: (1) create feed client, (2) wire into collector, (3) integrate into fair-value engine, (4) run source attribution replay, (5) enable if lift > threshold.

Candidates: Deribit vol structure, Polymarket cross-market, Synoptic HF-ASOS, additional funding sources, more CFB RTI constituent venues.

---

## Cross-Cutting: NATS Subject Hierarchy

```
market.kalshi.book.{ticker}           # from Rust
market.kalshi.trade.{ticker}          # from Rust
market.kalshi.status.{ticker}         # from Rust
data.crypto.binance.spot              # from Rust (existing Binance spot feed stays in Python for now, but NATS pub from Rust futures feed)
data.crypto.binance.futures           # from Rust
data.crypto.coinbase.spot             # from Rust
data.crypto.deribit.dvol              # from Rust (optional)
data.weather.observation.{station}    # from Python collector
data.weather.forecast.hrrr.{station}  # from Python collector
signal.{type}.{ticker}               # from Python evaluator
model.state.{ticker}                  # for UI/monitoring
```

Modify `python/signals/publisher.py` and `rust/src/execution.rs` (subscribe `signal.>` instead of `tradebot.signals`).

## Cross-Cutting: Redis Key Structure

```
orderbook:{ticker}          # Kalshi book (Rust)
model_state:{ticker}        # Model output (Python)
weather:maxmin:{station}    # Running daily max/min (Python)
crypto:shadow_rti           # Shadow RTI estimate (Python)
crypto:futures_basis        # Perp-spot basis (Python)
rules:{ticker}              # Resolved contract rules (Python)
feed:status:{feed_name}     # Feed health/staleness
```

---

## Implementation Sequencing

```
Phase 1: Contract Rules Resolver ──────┐  (Python, 1-2 days)
Phase 2: Kalshi Market State Engine ───┤  (Rust, 2-3 days, parallel with Phase 1)
                                       │
Phase 3: Weather Fair-Value Engine ────┤  (Python, 3-5 days, depends on Phase 1)
Phase 4: Crypto Fair-Value Engine ─────┤  (Rust feeds + Python model, 4-5 days, depends on Phase 1, parallel w/ Phase 3)
                                       │
Phase 5: Historical Capture & Replay ──┘  (Mixed, 2-3 days, depends on all above)
Phase 6: Gated Additions                  (ongoing, gated by Phase 5 attribution)
```

Note: Phase 4 is larger than originally estimated because crypto feeds (Coinbase, Binance futures, Deribit) are implemented in Rust with Redis bridge to Python models, rather than pure Python. This adds ~1 day but future-proofs for event-driven signal generation.

## Verification

After each phase:
- Run existing test suite: `just test-all`
- Weather Phase 3: backtest new model vs current ensemble, compare Brier scores
- Crypto Phase 4: backtest new model vs current N(d2), compare Brier scores
- Phase 5: run source ablation on at least 1 week of historical data
- End-to-end: run full pipeline in `PAPER_MODE=true`, verify signals flow from collector → evaluator → NATS → execution engine with new data sources visible in Redis/dashboard

## File Summary

**New files (21):**
- 4 migrations: `009_contract_rules.sql`, `010_weather_sources.sql`, `011_crypto_sources.sql`, `012_replay_tables.sql`
- `python/rules/` (5 files): `__init__.py`, `resolver.py`, `ticker_parser.py`, `timezone.py`, `discover.py`
- `python/data/` (2 new feeds): `aviationweather.py`, `open_meteo.py`
- `python/models/` (3 new models): `weather_fv.py`, `rounding.py`, `crypto_fv.py`
- `python/backtester/replay.py`
- `rust/src/kalshi/trade_tape.rs`
- `rust/src/feeds/` (4 files): `mod.rs`, `coinbase.rs`, `binance_futures.rs`, `deribit.rs`

**Modified files (15):**
- Rust: `rust/src/kalshi/websocket.rs`, `rust/src/kalshi/mod.rs`, `rust/src/orderbook_feed.rs`, `rust/src/main.rs`, `rust/src/config.rs`
- Python: `python/signals/types.py`, `python/signals/weather.py`, `python/signals/crypto.py`, `python/signals/publisher.py`, `python/evaluator/daemon.py`, `python/collector/daemon.py`, `python/data/kalshi_history.py`, `python/config.py`, `python/backtester/engine.py`
