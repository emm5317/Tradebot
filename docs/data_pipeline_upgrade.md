> **Status: COMPLETE** — All 5 phases of this pipeline upgrade have been implemented. Contract rules, Kalshi market state, weather FV, crypto FV, and historical replay are all operational. Test counts have grown beyond those listed here — see README.md for current totals.

# Data Pipeline Upgrade: Settlement-Focused Fair-Value Architecture

## Summary

Complete restructuring of the Tradebot data pipeline around 4 pillars:

1. **Contract rules / settlement correctness** — Exact mapping to resolution mechanics
2. **Kalshi-first market state** — Professional-grade local state engine with ticker channel, trade tape, stale detection
3. **Explicit fair-value engines** — Settlement-aware models for weather (NWS CLI) and crypto (CFB RTI)
4. **Historical replay & source attribution** — Prove each data source adds marginal value

## What Changed

### Phase 1: Contract Rules Resolver

**Problem:** Contract metadata was extracted via regex from English market titles (`_extract_threshold`, `_extract_station`, `_extract_city`). This couldn't handle settlement-source differences, DST, or max-vs-min contract distinction.

**Solution:** Structured ticker parser + `contract_rules` DB table.

| File | Purpose |
|------|---------|
| `migrations/009_contract_rules.sql` | `contract_rules` table with settlement source, station, timezone, strike, day boundaries |
| `python/rules/resolver.py` | `ContractRulesResolver` — loads from DB, caches by market_ticker |
| `python/rules/ticker_parser.py` | Parses `KXBTCD-26MAR08-T98500` format instead of English titles |
| `python/rules/timezone.py` | `compute_day_boundaries()` — local standard time, ignoring DST |
| `python/rules/discover.py` | One-time script to catalog all series ticker formats |

The resolver is used by the evaluator daemon to map each contract to its exact settlement mechanics. The `_infer_signal_type` keyword-matching function in the evaluator was replaced with a direct rules lookup.

### Phase 2: Kalshi Market State Engine (Rust)

**Problem:** Only `orderbook_delta` and `trade` channels were consumed. No ticker-level data (volume, OI, market status), no trade aggressiveness metrics, no stale detection.

**Solution:** Added ticker channel subscription, TradeTape, and enhanced Redis state.

| File | Purpose |
|------|---------|
| `rust/src/kalshi/trade_tape.rs` | Bounded `VecDeque<TradeRecord>` with `aggressiveness()`, `vwap()`, `recent_volume()` |
| `rust/src/kalshi/websocket.rs` | Added `TickerUpdate` variant, `"ticker"` channel subscription |
| `rust/src/orderbook_feed.rs` | Enhanced flush: trade_aggr_30s, volume, OI, market status, stale detection |
| `python/signals/types.py` | `OrderbookState` expanded with new Redis fields |

Redis key `orderbook:{ticker}` now includes: best_bid_size, best_ask_size, last_trade_price, trade_aggr_30s, recent_volume_60s, market_status, volume, open_interest. Market closed/settled status automatically clears the in-memory orderbook.

### Phase 3: Weather Fair-Value Engine

**Problem:** Gaussian diffusion ensemble didn't model settlement mechanics. Weather contracts settle on the NWS Daily Climate Report (CLI/DSM) using local standard time — the model had no concept of running daily max/min, no METAR 6-hourly group parsing, no HRRR forecasts, and no C→F rounding ambiguity handling.

**Solution:** Settlement-aware weather fair-value engine with 4 new data sources.

| File | Purpose |
|------|---------|
| `python/models/weather_fv.py` | `compute_weather_fair_value()` — running max/min tracking, lock detection, HRRR blending, rounding ambiguity |
| `python/models/rounding.py` | `compute_rounding_uncertainty()` — METAR C→F conversion ambiguity near threshold |
| `python/data/aviationweather.py` | METAR fetcher with `1xxxx`/`2xxxx` 6-hourly max/min group parsing |
| `python/data/open_meteo.py` | HRRR forecast fetcher (15-min resolution via Open-Meteo) |
| `migrations/010_weather_sources.sql` | `metar_observations`, `hrrr_forecasts`, `weather_daily_extremes` tables |

Key model logic:
- If running max already >= strike → probability locked at ~0.99 (outcome determined)
- HRRR forecast max vs strike → excursion probability
- METAR integer Celsius → Fahrenheit range straddles threshold → rounding ambiguity flag → widened uncertainty band, reduced confidence

### Phase 4: Crypto Fair-Value Engine

**Problem:** Bot used Binance spot + realized vol in Black-Scholes N(d2), but Kalshi settles to the CFB RTI (weighted mid from Coinbase, Bitstamp, Kraken, etc.). No constituent exchange data, no futures basis signal, no implied vol.

**Solution:** Rust exchange feeds + Python shadow RTI model.

**Rust feeds** (all config-gated, auto-reconnect, 500ms Redis flush):

| File | Feed | Redis Key |
|------|------|-----------|
| `rust/src/feeds/coinbase.rs` | Coinbase BTC-USD level2 | `crypto:coinbase` |
| `rust/src/feeds/binance_futures.rs` | Binance BTCUSDT perp/mark/funding | `crypto:binance_futures` |
| `rust/src/feeds/deribit.rs` | Deribit DVOL index | `crypto:deribit_dvol` |

**Python model:**

| File | Purpose |
|------|---------|
| `python/models/crypto_fv.py` | `compute_crypto_fair_value()` — shadow RTI, basis/funding signals, RTI averaging dampening |
| `migrations/011_crypto_sources.sql` | `crypto_ticks` table |

Shadow RTI = weighted average of Coinbase (0.6) + Binance (0.4). Coinbase gets higher weight as a known CFB RTI constituent.

### Phase 5: Historical Capture & Replay

**Problem:** No way to prove a data source adds edge. "Every new source must beat a 'does this improve out-of-sample calibration or PnL?' threshold."

**Solution:** Model evaluation persistence + replay engine with source ablation.

| File | Purpose |
|------|---------|
| `migrations/012_replay_tables.sql` | `kalshi_book_events` + `model_evaluations` tables |
| `python/backtester/replay.py` | `ReplayEngine` with `compute_attribution()` for Brier score / PnL deltas |
| `python/signals/publisher.py` | `publish_model_evaluation()` — persists full model output + input snapshots |

Usage: run baseline replay, then re-run with a source ablated. Compare Brier scores and PnL to measure the marginal lift of that source.

## Test Coverage

| Component | Tests | Framework |
|-----------|-------|-----------|
| Contract rules (ticker parser, timezone, resolver) | 35 | pytest |
| Weather fair-value (rounding, locking, HRRR, calibration, conflict) | 56 | pytest |
| Crypto fair-value (shadow RTI, basis, funding, DVOL, parity) | 36 | pytest |
| Existing tests (physics, evaluators, publisher, etc.) | 109 | pytest |
| Crypto state (venue weighting, staleness, outlier, volume) | 12 | cargo test |
| Crypto FV (N(d2), Levy, basis, confidence) | 18 | cargo test |
| Trade tape (aggressiveness, VWAP, volume, bounds) | 7 | cargo test |
| Orderbook (snapshots, deltas, staleness) | 6 | cargo test |
| Crypto feeds (Coinbase, Binance, Deribit parsing) | 14 | cargo test |
| Order manager, kill switch, feed health | 35 | cargo test |
| **Total** | **328** | |

## Migration Sequence

```
009_contract_rules.sql      # Contract settlement rules
010_weather_sources.sql      # METAR observations, HRRR forecasts, daily extremes
011_crypto_sources.sql       # Multi-exchange crypto ticks
012_replay_tables.sql        # Raw event capture + model evaluations
```

All migrations use `IF NOT EXISTS` and `ON CONFLICT` patterns — safe to re-run.
