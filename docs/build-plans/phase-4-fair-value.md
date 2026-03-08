# Phase 4 — Fair-Value Model Maturation

**Timeline:** Weeks 8–14
**Risk:** MEDIUM
**Goal:** Harden fair-value models with path-dependent settlement mechanics, dynamic venue weighting, and microstructure signals

---

## 4.1 Expiry-Window Path Dependence

### Problem
Current crypto model uses a single-point Gaussian probability, ignoring that RTI is a 60-second TWAP. Near expiry, the averaging window dampens tail risk — a sudden spike is diluted by 59 seconds of prior prices.

### Implementation

Extend `crypto_fv.rs` with RTI averaging model:
- Within final 60s of settlement window: model as running average, not point-in-time
- `prob = N(d2_adjusted)` where `d2_adjusted` accounts for averaging effect
- Use Levy's approximation for arithmetic average options
- For contracts >60s from expiry: standard Black-Scholes binary

**Backtest validation:**
- Compare new model Brier score vs current model on 1000+ historical settlements
- Must improve by >=2% to ship

---

## 4.2 Dynamic RTI Venue Weighting

### Problem
Shadow RTI uses fixed weights (60% Coinbase, 40% Binance). Real CFB RTI weights shift based on exchange volume, reliability, and outlier detection.

### Implementation

**Dynamic weights based on:**
1. **Volume share** (trailing 5 min): weight proportional to share of total volume
2. **Staleness penalty**: if a venue hasn't updated in >5s, reduce weight to 0
3. **Outlier detection**: if venue price deviates >0.5% from median, cap weight at 10%
4. **Minimum venues**: require >=2 healthy venues, else mark RTI as unreliable

**Config:**
- `RTI_MIN_VENUES=2`
- `RTI_OUTLIER_THRESHOLD_PCT=0.5`
- `RTI_STALE_THRESHOLD_SECS=5`

---

## 4.3 Kalshi Microstructure Layer

### Problem
Current model ignores orderbook dynamics: queue position, toxicity, spread regime changes.

### Implementation

**New signals from Kalshi orderbook:**
1. **Trade imbalance**: net aggressive buys vs sells from trade tape (last 30s)
2. **Spread regime**: classify as tight (<3%), normal (3-8%), wide (>8%)
3. **Depth imbalance**: bid depth / (bid + ask depth) — measures passive support
4. **Price momentum**: 30s price derivative from mid-price series

**Integration:**
- These signals adjust fair-value edge, not the model probability itself
- Tight spread + balanced depth → no adjustment
- Wide spread + aggressive selling → reduce buy-side edge by additional 5%
- Strong momentum aligned with signal → boost edge by 2%

---

## 4.4 Complete Rust Crypto Fair-Value Port

### Problem
After Phase 1.2 inlines the core N(d2) computation, remaining Python crypto logic (basis signal, funding signal, edge adjustments) should also move to Rust.

### Implementation

Port remaining logic from `python/models/crypto_fv.py`:
- Basis signal computation and interpretation
- Funding rate directional signal
- DVOL vs realized vol preference logic
- Edge threshold adjustments

After this, Python crypto evaluator can be fully decommissioned.

---

## 4.5 Station-Specific Calibration

### Problem
Weather model uses global sigma (volatility) and climatology tables. Different stations have different temperature distributions, diurnal patterns, and forecast skill.

### Implementation

**Per-station parameters:**
- `sigma_table[station][hour]`: station-specific hourly volatility
- `climo_table[station][month][hour]`: station-specific climatological mean
- `hrrr_skill[station]`: forecast skill score (0-1, how much to trust HRRR)
- `rounding_bias[station]`: systematic rounding bias direction

**Calibration process:**
- Use `replay_tables` historical data
- Compute per-station Brier scores
- Optimize ensemble weights per station (instead of global 35/25/20/20)
- Store in `calibration` table with station_id

---

## 4.6 Source Conflict and Outage Policy

### Problem
When METAR and HRRR disagree by >3°F, or when one source is unavailable, the model has no explicit policy.

### Implementation

**Conflict detection:**
- If `|metar_current - hrrr_forecast| > 3.0°F`: flag as conflict
- In conflict: increase model uncertainty (widen sigma by 50%)
- Log conflict for post-analysis

**Outage policy:**
- METAR unavailable: rely on HRRR + last known observation, increase sigma by 25%
- HRRR unavailable: rely on METAR + climatology, reduce HRRR weight to 0
- Both unavailable: refuse to generate signal (edge = 0)

---

## 4.7 Rounding Ambiguity Hardening

### Problem
METAR reports in °C (integer). Conversion to °F introduces rounding ambiguity near strike boundaries. Current `models/rounding.py` handles this but edge cases remain.

### Implementation

**Enhance rounding model:**
- Track both `floor(C*9/5+32)` and `ceil(C*9/5+32)` near half-degree boundaries
- When ambiguous (settlement could go either way): model as 50/50
- Identify "safe" zones where rounding cannot change settlement
- For strikes on exact conversion boundaries: apply beta distribution centered on 0.5

**Validation:**
- Backtest against all 2024–2025 weather settlements
- Identify cases where rounding ambiguity caused incorrect signals
- Brier score must not regress

---

## Verification Checklist

- [x] RTI averaging model improves Brier score on historical data
- [ ] Dynamic venue weights handle single-venue outage gracefully
- [ ] Microstructure signals measurably improve edge quality
- [ ] Rust crypto model matches Python output within 0.1% for all test cases
- [ ] Station-specific calibration improves per-station Brier scores
- [ ] Source conflict policy prevents signals on unreliable data
- [ ] Rounding ambiguity correctly identifies all half-degree boundaries
