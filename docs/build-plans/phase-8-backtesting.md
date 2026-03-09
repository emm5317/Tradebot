# Phase 8 — Advanced Backtesting & Adaptive Calibration

**Priority:** HIGH
**Risk:** LOW-MEDIUM (mostly Python, read-heavy; touches calibration feedback loop)
**Goal:** Fix broken calibration loop, add crypto sweep, transaction cost modeling, advanced metrics, parallel execution, and activate the unused replay engine

---

## Motivation

The calibration feedback loop (Phase 7) was designed to close the gap between predictions and outcomes, but **three bugs prevent it from functioning**:

1. `StationCalibration` constructor mismatch crashes every sweep run
2. Calibrator Job 4 queries non-existent DB columns, failing silently every hour
3. `model_evaluations` table is never populated, leaving the replay engine permanently empty

Additionally, the backtester lacks transaction cost modeling (overstating P&L), has no crypto parameter sweep, and runs sweeps sequentially. Phase 8 fixes the foundation and builds on it.

---

## 8.0 Fix Broken Calibration Pipeline (Prerequisites)

These bugs must be fixed before any other Phase 8 work.

### 8.0a — Fix `StationCalibration` constructor in sweep.py

**File:** `python/backtester/sweep.py` (lines 262-273)

The sweep constructs `StationCalibration` with individual keyword args (`weight_physics=`, `weight_hrrr=`, `hrrr_rmse_f=`, `sample_size=`) that don't exist on the dataclass. The dataclass (`physics.py:318-327`) has `weights: tuple[float,float,float,float]` as a single field.

**Fix:** Pass weights as a tuple:
```python
cal = StationCalibration(
    sigma_10min=0.3 * sigma_scale,
    hrrr_bias_f=0.0,
    hrrr_skill=0.7,
    rounding_bias=0.0,
    weights=(w_p, w_h, w_t, w_c),
)
```

### 8.0b — Fix calibrator Job 4 SQL query

**File:** `python/calibrator/daemon.py` (lines 214-225)

Job 4 queries `br.station` and `br.run_date` from `backtest_runs`, but migration 018 has neither column. The query fails every hourly cycle.

**Fix options (choose one):**
- **(A) Add `station` column to `backtest_runs`** via new migration, and populate it during sweep runs. This requires the sweep to run per-station (currently it runs globally).
- **(B) Rewrite Job 4** to extract station from `params` JSONB and use `created_at` instead of `run_date`:
  ```sql
  SELECT DISTINCT ON (params->>'station')
      params->>'station' AS station,
      brier_score AS sweep_brier,
      params
  FROM backtest_runs
  WHERE created_at > now() - interval '14 days'
    AND brier_score IS NOT NULL
    AND params->>'station' IS NOT NULL
  ORDER BY params->>'station', brier_score ASC
  ```
- **(C) Redesign the sweep to emit per-station runs** — requires sweeping each station separately and storing `station` in `backtest_runs`.

**Recommendation:** Option (C) with a new migration adding a `station` column to `backtest_runs`. This makes Job 4 clean and enables per-station weight optimization (which is the whole point). Migration also adds `run_date DATE` as a computed column default.

### 8.0c — Populate `avg_edge_realized` in rolling metrics

**File:** `python/calibrator/daemon.py` (line 144-199)

The `calibration_metrics.avg_edge_realized` column is never computed. Add to the rolling metrics query:
```sql
AVG(CASE WHEN o.outcome = 'win' THEN s.edge ELSE -s.edge END) AS avg_edge_realized
```

### 8.0d — Wire up `model_evaluations` population

**File:** `python/evaluator/daemon.py`

`publish_model_evaluation()` exists in `publisher.py` (line 123) but is never called. The replay engine (`replay.py`) reads from `model_evaluations` but it's always empty.

**Fix:** In the weather evaluator's evaluation loop, call `publish_model_evaluation()` for every contract evaluation (not just signals). This provides the data the replay engine needs for source attribution.

### 8.0e — Fix Brier score inconsistency between engine and sweep

**Files:** `python/backtester/engine.py` (line 177), `python/backtester/sweep.py`

The engine computes Brier on directional probability `p` (corrected for YES/NO direction), while the sweep computes Brier on raw `fv.probability`. Calibration buckets also use different conventions. Standardize both to use directional probability.

---

## 8.1 Crypto Parameter Sweep

### Problem

`CRYPTO_PARAM_GRID` is defined in `sweep.py` (line 45) but marked TBD. Crypto fair value is computed in Rust (`crypto_fv.rs`), making a full Python re-computation impractical.

### Design: Threshold Sweep on Stored Signals

Instead of re-computing crypto FV in Python (which would duplicate Rust math and be too slow for live use), sweep **threshold and sizing parameters** against historical signal data already stored in the `signals` table:

**Parameters to sweep:**
```python
CRYPTO_THRESHOLD_GRID = {
    "min_edge": [0.02, 0.03, 0.05, 0.07, 0.10],
    "min_confidence": [0.3, 0.4, 0.5, 0.6],
    "min_kelly": [0.01, 0.02, 0.03, 0.05],
    "kelly_multiplier": [0.25, 0.50, 0.75, 1.0],
}
```

**Flow:**
1. Fetch all crypto signals with `acted_on=true` or `acted_on=false` (both) from the sweep period
2. For each parameter combination, filter signals by thresholds
3. For surviving signals, compute simulated P&L using stored `model_prob`, `market_price`, `edge`, `kelly_fraction`
4. Apply `kelly_multiplier` scaling to compute position size
5. Compute Brier, accuracy, simulated P&L (with transaction costs from 8.2)

**Rationale:** This approach:
- Doesn't duplicate Rust math
- Runs fast (filtering stored signals, not re-computing FV)
- Optimizes the parameters that actually matter for execution quality
- Results feed into the Rust config via calibration agent (update `crypto_min_edge`, etc.)

### New function: `sweep_crypto()` and `_evaluate_crypto_thresholds()`

**File:** `python/backtester/sweep.py`

### Walk-forward support

Add crypto to the `walk_forward()` function (currently `continue # crypto walk-forward TBD` at line 190).

---

## 8.2 Transaction Cost Modeling

### Problem

The backtester's P&L simulation ignores trading fees, overstating profitability. Kalshi uses **three fee models** per series (from API: `fee_type` enum):
- `quadratic`: `fee = fee_multiplier × price × (1 - price) × count`
- `quadratic_with_maker_fees`: separate maker/taker quadratic fees
- `flat`: fixed fee per contract

### Implementation

**New module:** `python/backtester/costs.py`

```python
@dataclass
class FeeModel:
    """Kalshi fee model for backtesting."""
    fee_type: str = "quadratic"          # "quadratic", "flat"
    taker_fee_multiplier: float = 0.07   # 7% quadratic multiplier (Kalshi default)
    maker_fee_multiplier: float = 0.035  # 3.5% maker discount
    flat_fee_cents: int = 2              # flat fee fallback
    assume_taker: bool = True            # conservative: assume we take liquidity

    def compute_fee(self, price: float, count: int = 1) -> float:
        """Compute fee in cents for a trade."""
        if self.fee_type == "flat":
            return self.flat_fee_cents * count
        multiplier = self.taker_fee_multiplier if self.assume_taker else self.maker_fee_multiplier
        # Quadratic: fee = multiplier × price × (1 - price) × count × 100 (cents)
        return multiplier * price * (1.0 - price) * count * 100

    def round_trip_cost(self, entry_price: float, exit_price: float, count: int = 1) -> float:
        """Total fees for entry + exit (or entry + settlement)."""
        # Settlement is free on Kalshi; only entry has fees
        return self.compute_fee(entry_price, count)
```

**Integration points:**
- `engine.py:_compute_metrics()` — subtract fees from simulated P&L
- `sweep.py:_evaluate_weather_params()` — subtract fees from per-contract P&L
- `sweep.py:_evaluate_crypto_thresholds()` — subtract fees from per-signal P&L
- Fee model configurable via CLI args: `--fee-type quadratic --fee-multiplier 0.07`

### Fee source verification

Kalshi API returns `taker_fees` and `maker_fees` per order (in cents). For live reconciliation, compare backtested fees against actual fees from the `orders` table. Add a `backtested_fee_cents` column to signal detail for comparison.

---

## 8.3 Advanced Metrics

### Problem

Only accuracy, Brier score, and simple P&L are computed. Missing: log-loss, Sharpe ratio, max drawdown, time-decay weighting, calibration ECE.

### New metrics module: `python/backtester/metrics.py`

```python
@dataclass
class AdvancedMetrics:
    # Existing
    accuracy: float
    brier_score: float
    simulated_pnl_cents: int
    # New
    log_loss: float                      # -avg(y*log(p) + (1-y)*log(1-p))
    sharpe_ratio: float                  # annualized: mean(daily_returns)/std(daily_returns) * sqrt(252)
    sortino_ratio: float                 # like Sharpe but only downside deviation
    max_drawdown_cents: int              # peak-to-trough P&L decline
    max_drawdown_pct: float              # as percentage of peak
    profit_factor: float                 # gross_profit / gross_loss
    expected_calibration_error: float    # ECE: avg |predicted - actual| per bucket
    win_streak: int                      # longest consecutive wins
    loss_streak: int                     # longest consecutive losses
```

**Time-decay weighting:**
- Add optional exponential decay: recent signals weighted more heavily
- `weight = exp(-lambda * days_ago)` where `lambda` controls decay rate
- Default `lambda = 0.0` (no decay, backward compatible)
- Applied to Brier score and P&L computations
- CLI flag: `--time-decay 0.02` (2% daily decay)

**Per-day P&L series** (for Sharpe/drawdown):
- Group signals by settlement date
- Compute daily P&L after fees
- Sharpe = `mean(daily_pnl) / std(daily_pnl) * sqrt(252)`

### Storage

Add new columns to `backtest_runs`:
```sql
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS log_loss DOUBLE PRECISION;
-- log_loss column already exists in migration 018
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS sharpe_ratio DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS max_drawdown_cents BIGINT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS profit_factor DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS ece DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS fee_total_cents BIGINT DEFAULT 0;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS station TEXT;
```

---

## 8.4 Multi-Signal Evaluation per Contract

### Problem

Both `engine.py` (line 166) and `sweep.py` (line 366) `break` after the first valid signal per contract. This means:
- Only the earliest snapshot is evaluated
- If a better entry exists 5 minutes later, it's never seen
- The backtest doesn't capture "what if we waited?"

### Implementation

**New mode:** `--multi-signal` CLI flag (default: off for backward compatibility)

When enabled:
- Evaluate ALL snapshots per contract (don't break after first)
- Track per-contract: first signal, best-edge signal, last signal
- Metrics computed on best-edge signal by default (configurable)
- Signal detail includes all evaluations for drill-down

**New fields in `SignalRecord`:**
```python
@dataclass
class SignalRecord:
    # ... existing fields ...
    snapshot_index: int = 0          # which snapshot (0 = first)
    n_snapshots_evaluated: int = 1   # total snapshots for this contract
    is_best_edge: bool = True        # was this the best edge signal?
```

**Aggregate metrics:**
- `avg_signals_per_contract` — how many valid signals per contract on average
- `best_vs_first_edge` — average edge improvement from waiting for best signal

---

## 8.5 Parallel Sweep Execution

### Problem

Sweeps run sequentially. With 960 weather combos × per-station splits, a full sweep takes hours.

### Implementation: `ProcessPoolExecutor`

**File:** `python/backtester/sweep.py`

```python
from concurrent.futures import ProcessPoolExecutor, as_completed

async def sweep_weather_parallel(
    pool, start, end, max_workers=4, max_combos=200
):
    combos = _generate_combinations(WEATHER_PARAM_GRID, max_combos)
    # Pre-fetch all contracts once (shared across workers)
    contracts = await _fetch_settled_contracts(pool, start, end, ["weather"])

    results = []
    with ProcessPoolExecutor(max_workers=max_workers) as executor:
        futures = {
            executor.submit(_evaluate_weather_sync, contracts, params, start, end): params
            for params in combos
        }
        for i, future in enumerate(as_completed(futures)):
            result = future.result()
            results.append(result)
            if (i + 1) % 10 == 0:
                best = min(results, key=lambda r: r.brier_score or 999)
                logger.info("sweep_progress",
                    completed=i+1, total=len(combos),
                    best_brier=f"{best.brier_score:.4f}")

    # Store all results
    for result in results:
        await _store_run(pool, result, "weather", start, end)

    return sorted(results, key=lambda r: r.brier_score or 999)
```

**Key considerations:**
- Pre-fetch contracts + observations once, pass to workers as serialized data
- Each worker gets its own DB connection for METAR/HRRR fetches (or pre-fetch all)
- Progress reporting via `as_completed` callback
- CLI flag: `--workers 4` (default: `min(4, cpu_count)`)

**Data pre-fetching strategy:**
- Contracts, ASOS observations, METAR, HRRR forecasts are all read-only
- Fetch ALL relevant data up-front into a dict keyed by `(ticker, snapshot_time)`
- Pass the data dict to each worker (avoids per-worker DB connections)
- This also speeds up sequential sweeps by eliminating per-contract DB queries

---

## 8.6 Replay Engine Activation

### Problem

`replay.py` is fully built but:
1. `model_evaluations` table is never populated (fixed in 8.0d)
2. `ablation_sources` parameter is accepted but not applied
3. `model_fn` parameter is accepted but not used
4. No CLI entry point
5. No integration with the calibration agent

### Implementation

### 8.6a — Implement source ablation in replay

**File:** `python/backtester/replay.py`

When `ablation_sources` is non-empty, filter the `components` JSONB to zero out specified sources and re-compute the blended probability:

```python
async def replay(self, config, model_fn=None):
    # ... existing row fetch ...
    for row in rows:
        if config.ablation_sources and row["components"]:
            # Re-blend without ablated sources
            components = json.loads(row["components"]) if isinstance(row["components"], str) else row["components"]
            model_prob = self._ablate_and_reblend(components, config.ablation_sources)
        else:
            model_prob = row["model_prob"]
        # ... rest of scoring ...
```

### 8.6b — CLI entry point for replay

Add replay commands to the backtester CLI:
```bash
python -m backtester.replay --start 2026-01-01 --end 2026-03-01 --type weather
python -m backtester.replay --start 2026-01-01 --end 2026-03-01 --type weather --ablate hrrr
python -m backtester.replay --attribution  # run baseline + ablation for each source
```

### 8.6c — Calibrator integration

Add a Job 7 to `calibrator/daemon.py`: monthly source attribution report. Runs replay with each source ablated, computes marginal lift, logs results. If a source consistently has negative lift, alert via Discord.

---

## 8.7 Comprehensive Test Coverage

### Problem

`test_sweep.py` tests only utility functions (combinations, walk-forward splits), not the actual evaluation pipeline. No integration tests for the sweep→calibrate→evaluate cycle.

### New tests

**`python/tests/test_backtester_metrics.py`:**
- Log-loss computation (known values)
- Sharpe ratio (flat returns → 0, positive returns → positive)
- Max drawdown (peak-to-trough)
- ECE (perfectly calibrated model → 0)
- Time-decay weighting (recent signals weighted more)
- Profit factor (gross wins / gross losses)

**`python/tests/test_costs.py`:**
- Quadratic fee at 50 cents (maximum fee)
- Quadratic fee near 0 or 100 cents (near-zero fee)
- Flat fee model
- Round-trip cost computation
- Fee model from config

**`python/tests/test_sweep_evaluation.py`:**
- `StationCalibration` construction with weights tuple (regression test for 8.0a)
- `_evaluate_weather_params()` with mocked data
- Crypto threshold sweep with mocked signals
- Walk-forward split generation (existing, expand edge cases)
- Multi-signal evaluation mode

**`python/tests/test_replay_engine.py`:**
- Replay with populated model_evaluations
- Source ablation re-blending
- Attribution computation (positive lift, negative lift, neutral)

**`python/tests/test_calibrator_integration.py`:**
- Job 4 with real `backtest_runs` schema (regression test for 8.0b)
- Station weight update when sweep beats baseline
- No update when improvement < 0.005 threshold

---

## New Migration: 020_backtest_enhancements.sql

```sql
-- Phase 8: Backtester enhancements
-- Add station column and advanced metrics to backtest_runs

ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS station TEXT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS sharpe_ratio DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS max_drawdown_cents BIGINT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS profit_factor DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS ece DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS fee_total_cents BIGINT DEFAULT 0;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS time_decay_lambda DOUBLE PRECISION DEFAULT 0.0;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS n_signals_per_contract DOUBLE PRECISION;

CREATE INDEX IF NOT EXISTS idx_backtest_runs_station
    ON backtest_runs (station, brier_score) WHERE station IS NOT NULL;

-- Add run_date as an alias for easy querying (derived from created_at)
-- This fixes the calibrator Job 4 reference to br.run_date
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS run_date DATE
    GENERATED ALWAYS AS (created_at::date) STORED;
```

---

## Files Modified / Created

| File | Change | Phase |
|------|--------|-------|
| `python/backtester/sweep.py` | Fix StationCalibration constructor, add crypto sweep, parallel execution, per-station runs | 8.0a, 8.1, 8.5 |
| `python/calibrator/daemon.py` | Fix Job 4 SQL, add avg_edge_realized, add Job 7 attribution | 8.0b, 8.0c, 8.6c |
| `python/evaluator/daemon.py` | Wire up publish_model_evaluation() | 8.0d |
| `python/backtester/engine.py` | Fix Brier consistency, multi-signal mode, integrate fees/metrics | 8.0e, 8.4, 8.2, 8.3 |
| `python/backtester/costs.py` | **New** — Kalshi fee model (quadratic/flat) | 8.2 |
| `python/backtester/metrics.py` | **New** — Advanced metrics (log-loss, Sharpe, drawdown, ECE) | 8.3 |
| `python/backtester/replay.py` | Implement ablation, add CLI entry point | 8.6a, 8.6b |
| `migrations/020_backtest_enhancements.sql` | **New** — station column, advanced metric columns | 8.3 |
| `python/tests/test_backtester_metrics.py` | **New** — advanced metrics tests | 8.7 |
| `python/tests/test_costs.py` | **New** — fee model tests | 8.7 |
| `python/tests/test_sweep_evaluation.py` | **New** — sweep evaluation pipeline tests | 8.7 |
| `python/tests/test_replay_engine.py` | **New** — replay engine tests | 8.7 |
| `python/tests/test_calibrator_integration.py` | **New** — calibrator Job 4 regression tests | 8.7 |

---

## Implementation Chunks

### Chunk A: Foundation Fixes (Session 1)
| Step | Section | Impact | Effort |
|------|---------|--------|--------|
| 1 | 8.0a | Unblocks all sweeps | Small |
| 2 | 8.0b | Unblocks calibration feedback loop | Medium |
| 3 | 8.0c | Completes rolling metrics | Small |
| 4 | 8.0d | Activates replay engine data pipeline | Small |
| 5 | 8.0e | Consistent Brier scoring | Small |

### Chunk B: Transaction Costs + Advanced Metrics (Session 2)
| Step | Section | Impact | Effort |
|------|---------|--------|--------|
| 6 | 8.2 | Realistic P&L simulation | Medium |
| 7 | 8.3 | Richer decision-making metrics | Medium |
| 8 | Migration 020 | DB schema for new columns | Small |

### Chunk C: Crypto Sweep + Parallel Execution (Session 3)
| Step | Section | Impact | Effort |
|------|---------|--------|--------|
| 9 | 8.1 | Crypto parameter optimization | Medium |
| 10 | 8.5 | 4-8x sweep speedup | Medium |
| 11 | 8.4 | Better entry point selection | Medium |

### Chunk D: Replay Engine + Tests (Session 4)
| Step | Section | Impact | Effort |
|------|---------|--------|--------|
| 12 | 8.6a-b | Source attribution working | Medium |
| 13 | 8.6c | Automated source quality monitoring | Small |
| 14 | 8.7 | Comprehensive test coverage | Medium |

---

## Verification Checklist

### Chunk A
- [ ] `python -m backtester.sweep --type weather --start 2026-01-01 --end 2026-02-01` runs without TypeError
- [ ] After 1 hour, `SELECT * FROM calibration_metrics WHERE avg_edge_realized IS NOT NULL` returns rows
- [ ] Calibrator Job 4 logs show successful station weight comparisons (no SQL errors)
- [ ] `SELECT COUNT(*) FROM model_evaluations` grows after evaluator runs for 1 hour
- [ ] Engine and sweep produce identical Brier scores on same data

### Chunk B
- [ ] P&L with fees < P&L without fees (fees are being subtracted)
- [ ] Quadratic fee at price=0.50 is maximal, at price=0.01 is near-zero
- [ ] `backtest_runs` has non-null `sharpe_ratio`, `max_drawdown_cents` after sweep
- [ ] Time-decay λ=0 produces same results as no decay (backward compat)

### Chunk C
- [ ] `python -m backtester.sweep --type crypto` runs and produces results
- [ ] Parallel sweep with `--workers 4` produces same best params as sequential
- [ ] Multi-signal mode shows avg_signals_per_contract > 1 for most contracts
- [ ] Crypto walk-forward produces train/val splits with overfit ratios

### Chunk D
- [ ] `python -m backtester.replay --type weather --start ... --end ...` returns results
- [ ] `--ablate hrrr` produces different Brier than baseline
- [ ] `--attribution` ranks data sources by marginal lift
- [ ] All new test files pass: `pytest python/tests/test_backtester_metrics.py test_costs.py test_sweep_evaluation.py test_replay_engine.py test_calibrator_integration.py -v`
- [ ] Total test count increases from 322 to ~380+
