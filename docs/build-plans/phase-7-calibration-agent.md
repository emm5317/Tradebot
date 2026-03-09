# Phase 7 — Calibration Agent & Prediction Feedback Loop

**Priority:** CRITICAL
**Risk:** LOW (read-heavy; writes only to calibration/config tables)
**Goal:** Close the feedback loop between predictions and settlement outcomes so models self-improve over time

---

## Motivation

After Phases 0–6, the system generates signals, executes trades, and stores outcomes — but **nothing learns from the results**. Data flows in one direction: signals → storage → reports. The daily Brier scores in `strategy_performance` are always NULL due to missing linkage bugs, `station_calibration` weights are permanently at defaults, and walk-forward sweep results are never applied. The system is flying blind on whether its predictions are calibrated.

This phase closes the loop with an autonomous calibration agent and fixes the data plumbing bugs that prevent any feedback from functioning.

---

## 7.0 Fix Data Plumbing Bugs (Prerequisites)

These bugs must be fixed before any calibration work can function.

### 7.0a — Write `signal_id` on orders

**File:** `rust/src/order_manager.rs`, `persist_order()` (~line 1129)

The INSERT into `orders` omits `signal_id`, so the Brier score JOIN in `aggregator.py` (`JOIN orders o ON o.signal_id = s.id`) always returns zero rows.

**Fix:** Pass the originating signal's DB id through `ManagedOrder` and bind it in the INSERT.

### 7.0b — Write `outcome` on orders after settlement

**File:** `rust/src/order_manager.rs`

The `orders.outcome` column (migration 003) is never set to `'win'` or `'loss'`. The reconciliation loop or a post-settlement sweep must UPDATE orders based on `contracts.settled_yes`.

**Fix:** Add a `settle_orders()` method that runs after contract sync, joining `orders` against `contracts` to set `outcome = CASE WHEN (side='yes' AND settled_yes) OR (side='no' AND NOT settled_yes) THEN 'win' ELSE 'loss' END`.

### 7.0c — Write `latency_ms` on orders

**File:** `rust/src/order_manager.rs`, `persist_order()` (~line 1160)

`latency_ms` is correctly measured via `submit_start.elapsed()` and logged, but the DB INSERT hardcodes `0i32`. Fix: pass the measured value through.

### 7.0d — Call `compute_hrrr_skill_scores()`

**File:** `python/models/physics.py` (line 375)

This function computes HRRR bias, RMSE, and skill scores per station from actual observations, and upserts into `station_calibration`. It is fully implemented but has **zero call sites**. HRRR skill is permanently the default 0.5 for every station.

**Fix:** Call it from the calibration agent on each hourly cycle.

---

## 7.1 Calibration Agent Daemon

**New file:** `python/calibrator/daemon.py`

A Python daemon that runs on an hourly cycle, performing four jobs:

### Job 1: Settle Order Outcomes

```sql
UPDATE orders o
SET outcome = CASE
    WHEN (o.side = 'yes' AND c.settled_yes = true)
      OR (o.side = 'no'  AND c.settled_yes = false)
    THEN 'win' ELSE 'loss'
  END
FROM contracts c
WHERE o.ticker = c.ticker
  AND c.settled_yes IS NOT NULL
  AND o.outcome = 'pending';
```

### Job 2: Populate Calibration Table

Join `signals` against `contracts.settled_yes` to write prediction-vs-outcome records into the `calibration` hypertable (migration 007), which has existed since Phase 2 but has never been populated.

```sql
INSERT INTO calibration (signal_id, ticker, signal_type, model_prob, actual_outcome, prob_bucket)
SELECT s.id, s.ticker, s.signal_type, s.model_prob,
       CASE WHEN c.settled_yes THEN 1.0 ELSE 0.0 END,
       ROUND(s.model_prob * 10) / 10  -- bucket to nearest 0.1
FROM signals s
JOIN contracts c ON s.ticker = c.ticker
WHERE c.settled_yes IS NOT NULL
  AND s.id NOT IN (SELECT signal_id FROM calibration)
ON CONFLICT DO NOTHING;
```

### Job 3: Compute Rolling Metrics

Per (strategy, station, hour, month):
- Rolling 30-day Brier score
- Rolling 30-day calibration curve (predicted vs actual per probability bucket)
- Rolling 30-day edge realization (predicted edge vs actual profit)
- Rolling 30-day slippage (fill_price - market_price_at_order)

Store results in `strategy_performance` (existing table) and a new `calibration_metrics` table.

### Job 4: Update Station Calibration Weights

Compare current `station_calibration` weights against recent walk-forward sweep results in `backtest_runs`:

```python
async def maybe_update_weights(pool, station, month, hour):
    """Update station_calibration if recent sweep found better weights."""
    current = await get_current_weights(pool, station, month, hour)
    recent_sweep = await get_best_recent_sweep(pool, station, days=14)

    if recent_sweep is None:
        return

    # Only update if sweep Brier score beats current by >0.005
    if recent_sweep.brier_score < current.brier_score - 0.005:
        await update_station_calibration(
            pool, station, month, hour,
            weights=recent_sweep.params,
        )
        logger.info("calibration_weights_updated", station=station,
                     old_brier=current.brier_score,
                     new_brier=recent_sweep.brier_score)
```

### Job 5: HRRR Skill Recalculation

Call `compute_hrrr_skill_scores()` from `physics.py` to update HRRR bias/RMSE/skill in `station_calibration` based on recent observations.

### Job 6: Drift Detection & Alerting

Detect model degradation and alert via Discord webhook:

```python
async def check_drift(pool, webhook_url):
    """Alert if any strategy's Brier score degrades significantly."""
    for strategy in ["weather", "crypto"]:
        recent = await rolling_brier(pool, strategy, days=7)
        baseline = await rolling_brier(pool, strategy, days=30)

        if recent is None or baseline is None:
            continue

        if recent > baseline + 0.03:  # Brier degradation > 3%
            await send_discord_alert(webhook_url,
                f"Model drift: {strategy} 7d Brier {recent:.3f} "
                f"vs 30d baseline {baseline:.3f}")
```

### Daemon Loop

```python
class CalibrationDaemon:
    async def run(self):
        while True:
            try:
                await self.settle_order_outcomes()
                await self.populate_calibration_table()
                await self.compute_rolling_metrics()
                await self.update_station_weights()
                await self.recalculate_hrrr_skill()
                await self.check_drift()
                logger.info("calibration_cycle_complete")
            except Exception:
                logger.exception("calibration_cycle_failed")

            await asyncio.sleep(3600)  # hourly
```

---

## 7.2 Execution Quality Tracking

### 7.2a — Slippage Measurement

**New view or materialized query:**

```sql
SELECT signal_type,
       AVG(fill_price - market_price_at_order) AS avg_slippage,
       PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY ABS(fill_price - market_price_at_order)) AS p95_slippage
FROM orders
WHERE outcome IN ('win', 'loss')
GROUP BY signal_type;
```

The calibration agent should compute this per-strategy and feed it back to adjust fill price estimates. If average slippage is +0.02, the evaluator's `estimate_fill_price()` should add a 0.02 penalty.

### 7.2b — Use Book-Walking Fill Estimation

**File:** `rust/src/kalshi/orderbook.rs` (line 146)

`estimated_fill_price(side, size)` is fully implemented and tested — it walks the orderbook to compute true VWAP fill cost for a given order size. Currently unused; the evaluator uses `mid ± spread/2` instead.

**Fix:** Replace `estimate_fill_price()` in `crypto_fv.rs:159` with a call to the orderbook's `estimated_fill_price()`, passing the anticipated order size. This gives more accurate Kelly fractions on thin books where `mid ± spread/2` significantly underestimates actual fill cost.

---

## 7.3 Exploit Unused Market Data

The system collects rich market data that is currently discarded or ignored in signal logic:

### 7.3a — Kalshi Price Momentum

**Files:** `rust/src/kalshi/orderbook_feed.rs`, `rust/src/crypto_evaluator.rs`

`TickerUpdate.last_price` and `last_trade_count` are received over WebSocket but silently dropped in the orderbook feed handler. Track a rolling window of Kalshi mid-price changes to detect:
- **Convergence**: market moving toward model FV → opportunity closing, fire immediately
- **Divergence**: market moving away from model FV → opportunity growing, consider waiting

Add to `compute_microstructure_adj()` as a momentum component.

### 7.3b — Trade Tape VWAP Signal

**File:** `rust/src/kalshi/trade_tape.rs` (line 82)

`TradeTape.vwap()` is implemented and tested but never called. VWAP above current mid indicates buying pressure (bullish for YES); below mid indicates selling pressure.

Add as a microstructure signal: `vwap_signal = (vwap - mid) / spread`, clamped to ±0.02.

### 7.3c — Volume Surge Detection

**File:** `rust/src/kalshi/trade_tape.rs` (line 71)

`recent_volume(60s)` is flushed to Redis but not used in evaluators. A sudden volume spike often precedes informed trading. When volume exceeds 3x the 5-minute average, it signals that someone with information is acting — the evaluator should increase confidence or widen the entry window.

### 7.3d — Open Interest Changes

Track `TickerUpdate.open_interest` delta between cycles. Rising OI with price movement confirms the trend; rising OI without movement suggests accumulation before a move.

---

## 7.4 Confidence-Scaled Order Sizing

**File:** `rust/src/order_manager.rs`, `compute_order_size()` (line 1084)

Currently, confidence gates entry (pass/fail at `crypto_min_confidence`) but does not scale order size. A 0.90 confidence signal gets the same Kelly fraction as a 0.55 confidence signal.

**Fix:**

```rust
fn compute_order_size(config: &Config, signal: &Signal, confidence: f64) -> i64 {
    let kelly_adjusted = signal.kelly_fraction * config.kelly_fraction_multiplier;
    // Scale by confidence: 0.5 confidence → 50% size, 1.0 → 100%
    let confidence_scale = confidence.clamp(0.3, 1.0);
    let size = (kelly_adjusted * confidence_scale * 10000.0) as i64;
    size.min(config.max_trade_size_cents).max(1)
}
```

This requires propagating `confidence` through the signal path (it's already computed in both `crypto_fv.rs` and `weather_fv.py` but not included in the `Signal` struct sent over NATS).

---

## 7.5 Execution Timing Optimization

### Edge Trajectory Tracking

**File:** `rust/src/crypto_evaluator.rs`

Currently, the first qualifying tick in the entry window fires immediately. Add edge trajectory analysis:

```rust
struct EdgeTracker {
    history: VecDeque<(Instant, f64)>,  // (time, edge) pairs
}

impl EdgeTracker {
    fn trend(&self, window_secs: u64) -> f64 {
        // Linear regression slope of edge over recent window
        // Positive = edge growing, negative = edge shrinking
    }

    fn should_wait(&self) -> bool {
        let slope = self.trend(30);
        // If edge is growing fast, wait up to 15 seconds
        slope > 0.001 && self.history.len() < 30
    }
}
```

When edge is increasing (model diverging from market), waiting briefly captures more. When edge is decreasing (market converging), fire immediately.

---

## 7.6 Evaluator Hot-Reload of Calibration

**File:** `python/evaluator/daemon.py`

The evaluator loads `sigma_table`, `climo_table`, and `station_calibration` once at startup and never refreshes. Even if the calibration agent updates weights, the running evaluator won't see them until restart.

**Fix:** Add a 15-minute refresh cycle:

```python
async def _maybe_refresh_calibration(self):
    """Reload calibration tables if stale."""
    if (time.monotonic() - self._last_cal_refresh) > 900:  # 15 min
        self.sigma_table = await build_sigma_table(self.pool)
        self.climo_table = await build_climo_table(self.pool)
        self.station_cal = await build_station_calibration(self.pool)
        self._last_cal_refresh = time.monotonic()
        logger.info("calibration_tables_refreshed")
```

---

## Docker Compose

```yaml
  calibrator:
    build:
      context: ..
      dockerfile: Dockerfile
    depends_on:
      postgres:
        condition: service_healthy
      migrate:
        condition: service_completed_successfully
    environment:
      DATABASE_URL: postgres://tradebot:${POSTGRES_PASSWORD:-tradebot_dev}@postgres:5432/tradebot
      DISCORD_WEBHOOK_URL: ${DISCORD_WEBHOOK_URL:-}
      LOG_LEVEL: info
    command: ["python", "-m", "calibrator.daemon"]
    restart: unless-stopped
```

---

## New Migration: 019_calibration_metrics.sql

```sql
-- Rolling calibration metrics computed by calibration agent
CREATE TABLE IF NOT EXISTS calibration_metrics (
    id              BIGSERIAL PRIMARY KEY,
    strategy        TEXT NOT NULL,
    station         TEXT,
    hour            SMALLINT,
    month           SMALLINT,
    period_start    DATE NOT NULL,
    period_end      DATE NOT NULL,
    brier_score     REAL,
    avg_predicted   REAL,
    avg_actual      REAL,
    signal_count    INTEGER NOT NULL DEFAULT 0,
    avg_slippage    REAL,
    p95_slippage    REAL,
    avg_edge_realized REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cal_metrics_strategy_period
    ON calibration_metrics (strategy, period_end DESC);
```

---

## Files Modified / Created

| File | Change |
|------|--------|
| `rust/src/order_manager.rs` | Fix `signal_id`, `outcome`, `latency_ms` in persist_order() |
| `python/calibrator/__init__.py` | **New** — package init |
| `python/calibrator/daemon.py` | **New** — calibration agent daemon |
| `python/evaluator/daemon.py` | Add 15-min calibration hot-reload |
| `python/models/physics.py` | No changes (already has `compute_hrrr_skill_scores`) |
| `rust/src/crypto_evaluator.rs` | Edge trajectory tracking, use VWAP/volume signals |
| `rust/src/crypto_fv.rs` | Use book-walking `estimated_fill_price()` |
| `rust/src/kalshi/orderbook_feed.rs` | Stop discarding `last_price`/`last_trade_count` |
| `docker/docker-compose.yml` | Add calibrator service |
| `migrations/019_calibration_metrics.sql` | **New** — rolling metrics table |

---

## Verification

1. **Bug fixes**: After deploying 7.0a-d, verify `orders.signal_id` is populated, `orders.outcome` updates to win/loss after settlement, `latency_ms` is non-zero.
2. **Calibration agent**: After 24h of running, verify `calibration` table has rows, `calibration_metrics` has rolling scores, `station_calibration` weights have been updated for at least one station.
3. **Drift detection**: Manually degrade a model weight and verify Discord alert fires within 1 hour.
4. **Hot-reload**: Update `station_calibration` via psql while evaluator is running; verify new weights take effect within 15 minutes (check logs for `calibration_tables_refreshed`).
5. **Execution quality**: After 100+ trades, query slippage metrics and verify `estimated_fill_price()` book-walking produces tighter estimates than `mid ± spread/2`.

---

## Priority Sequence

| Step | Section | Impact | Effort |
|------|---------|--------|--------|
| 1 | 7.0a-d | Unblocks everything | Small (bug fixes) |
| 2 | 7.1 | Closes feedback loop | Medium (new daemon) |
| 3 | 7.6 | Models adapt without restart | Small |
| 4 | 7.2b | Better fill estimates | Small |
| 5 | 7.4 | Right-sizes trades | Small |
| 6 | 7.3a-d | New signal inputs | Medium |
| 7 | 7.5 | Optimal trade timing | Medium |
