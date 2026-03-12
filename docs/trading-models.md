# Trading Models

Detailed documentation for Tradebot's fair-value models. For a high-level overview, see the [README](../README.md).

## Weather Contracts — Settlement-Aware Fair Value

Kalshi weather contracts settle on the **NWS Daily Climate Report** (CLI/DSM), which uses **local standard time** (not DST). The model tracks settlement mechanics directly:

1. **Running max/min tracking** — Maintains daily running maximum (or minimum) temperature from all observations throughout the settlement day
2. **Lock detection** — If the running max already exceeds the strike, probability locks at ~0.99 (the day's high has been recorded)
3. **METAR 6-hourly groups** — Parses `1xxxx`/`2xxxx` remark groups that feed directly into the Daily Climate Report
4. **HRRR forecast blending** — High-resolution (15-min) HRRR forecasts from Open-Meteo for remaining-day excursion probability, with per-station bias correction
5. **C-to-F rounding ambiguity** — METAR reports Celsius; CLI reports Fahrenheit. Near threshold boundaries, the integer rounding creates settlement ambiguity. The model computes boundary probability using a uniform distribution over the possible Fahrenheit range and identifies "safe zones" where rounding cannot affect the outcome
6. **Station-specific calibration** — Per-(station, month, hour) sigma from historical observations, HRRR skill scoring (1 - RMSE/climo_std), and optimized ensemble weights per station
7. **Source conflict detection** — When METAR and HRRR disagree by >3F, sigma is inflated 50%. METAR outage inflates sigma 25%. Both missing yields low-confidence 0.5 probability
8. **Gaussian diffusion ensemble** — Physics, HRRR, trend, and climatology components with station-calibrated weights

Default component weights: 35% physics, 25% HRRR, 20% trend, 20% climatology (overridden by station-specific calibration when available).

### Key Files

| File | Purpose |
|------|---------|
| `python/models/weather_fv.py` | Settlement-aware weather fair value |
| `python/models/physics.py` | Gaussian ensemble + StationCalibration |
| `python/models/rounding.py` | METAR C-to-F rounding ambiguity + boundary probability |
| `python/rules/resolver.py` | Contract rules resolver (settlement mapping) |
| `python/evaluator/daemon.py` | Weather evaluation loop (10s cycle) |

## Crypto Contracts — Event-Driven Fair Value (Rust)

Kalshi crypto contracts settle to **CF Benchmarks Real-Time Indices** (CFB RTI/ERTI) — a 60-second weighted average from constituent exchanges (Coinbase, Bitstamp, Kraken, etc.). The bot supports **5 assets**: BTC, ETH, SOL, XRP, and DOGE, each with per-asset tuning via `AssetConfig` (default volatility, vol multiplier, excess kurtosis).

1. **Dynamic RTI estimation** — Volume-weighted average of constituent exchange spot prices with staleness detection (>5s = weight 0), outlier capping (>0.5% deviation from median = weight capped at 10%), and reliability flagging (requires 2+ healthy venues). Each asset has its own `CryptoState` instance via `CryptoStateRegistry`
2. **Gaussian probability** — N(d2) model using shadow RTI, time-scaled volatility, and the contract strike. Volatility is scaled by a per-asset `binary_vol_multiplier` (BTC: 2.5, ETH: 2.8, SOL: 3.0, XRP: 3.0, DOGE: 3.2) to account for jump risk and fat tails
3. **Kurtosis tail correction** — Per-asset excess kurtosis (BTC: 7.0, ETH: 8.0, SOL: 10.0, XRP: 9.0, DOGE: 12.0). For deep ITM/OTM brackets (z-score > 2.0), a tail adjustment prevents overconfident probabilities. All probabilities clamped to [PROB_FLOOR, PROB_CEILING]
4. **Levy averaging near expiry** — Within the final 60s, the RTI averaging window dampens tail risk. Uses Levy's approximation for arithmetic average options to model effective strike shift and volatility reduction
5. **Basis signal** — Perpetual futures vs spot basis indicates directional sentiment (BTC-only, via Binance futures)
6. **Funding rate signal** — Positive funding (longs pay shorts) signals bullish market structure (BTC-only)
7. **Deribit DVOL** — Market-implied volatility from the BTC volatility index, preferred over realized vol when available (BTC-only)
8. **Microstructure adjustments** — Trade tape aggressiveness (+/-2%), spread regime penalties (tight: +1%, wide: -2%), depth imbalance (+/-2%), clamped to +/-4% total

### Multi-Asset Architecture

- `CryptoAsset` enum: BTC, ETH, SOL, XRP, DOGE — derived from ticker prefix (KXBTC, KXETH, etc.)
- `CryptoStateRegistry`: one `CryptoState` per enabled asset, merged watch channel for evaluator
- Coinbase/Binance spot feeds subscribe to all enabled assets in a single WebSocket
- Binance futures and Deribit remain BTC-only (only BTC has perps/DVOL on these venues)
- Per-asset enable flags: `ENABLE_CRYPTO_BTC` (default true), `ENABLE_CRYPTO_ETH/SOL/XRP/DOGE` (default false)

### Key Files

| File | Purpose |
|------|---------|
| `rust/src/crypto_asset.rs` | CryptoAsset enum and per-asset symbol mappings |
| `rust/src/crypto_state.rs` | CryptoState with dynamic venue weighting (RTI) |
| `rust/src/crypto_state_registry.rs` | Per-asset CryptoState registry with merged notify |
| `rust/src/crypto_fv.rs` | AssetConfig, N(d2), Levy averaging, basis, funding |
| `rust/src/crypto_evaluator.rs` | Event-driven crypto eval + microstructure (multi-asset) |
| `rust/src/feeds/` | Coinbase, Binance Spot/Futures, Deribit WebSocket feeds |

## Shared Signal Logic

- Spread-adjusted edge with wide-spread penalty (15% discount above 10% spread)
- Max edge filter — signals with edge > 25% are rejected as model miscalibration rather than genuine alpha
- Kelly criterion sizing using estimated fill price (best ask for YES, best bid for NO)
- Signal cooldown (crypto: 30s, weather: 120s per ticker) to prevent duplicate entries
- Exit signals when edge flips below -3%
