# Phase 7 Implementation Prompt

Copy everything below the line into a new Claude Code session.

---

Implement Phase 7 from `docs/build-plans/phase-7-calibration-agent.md`. Do steps 7.0 through 7.6 in order. Here is the exact sequence:

## Step 1: Fix data plumbing bugs (7.0a-d)

These are prerequisites — nothing downstream works without them.

### 7.0a — Write `signal_id` on orders

In `rust/src/order_manager.rs`, the `persist_order()` INSERT into `orders` omits `signal_id`. The signal's DB id needs to flow through `ManagedOrder` and be bound in the INSERT. The `signals` table has an `id BIGSERIAL PRIMARY KEY` — when a signal is persisted to the DB (check `persist_signal()` or the NATS→DB path), capture that id and attach it to the resulting `ManagedOrder` so `persist_order()` can write it.

### 7.0b — Settle order outcomes

Add a `settle_order_outcomes()` function (can live in `order_manager.rs` or a new Rust module) that runs periodically:

```sql
UPDATE orders SET outcome = CASE
  WHEN (side='yes' AND c.settled_yes=true) OR (side='no' AND c.settled_yes=false) THEN 'win'
  ELSE 'loss'
END
FROM contracts c WHERE orders.ticker = c.ticker AND c.settled_yes IS NOT NULL AND orders.outcome = 'pending';
```

Wire this into the existing reconciliation loop or run it on a timer.

### 7.0c — Fix latency_ms

In `rust/src/order_manager.rs` `persist_order()`, the `latency_ms` column is hardcoded to `0i32`. Pass the actual measured `submit_start.elapsed().as_millis()` value through `ManagedOrder` and bind it in the INSERT.

### 7.0d — Call `compute_hrrr_skill_scores()`

In `python/models/physics.py`, `compute_hrrr_skill_scores()` (around line 375) is fully implemented but never called. It will be called by the calibration agent in step 2, so just verify it exists and works — don't add a call site yet.

## Step 2: Build calibration agent (7.1)

Create `python/calibrator/__init__.py` and `python/calibrator/daemon.py`.

The daemon runs an hourly loop with these jobs:
1. **Settle outcomes** — Run the SQL from 7.0b (Python version using asyncpg)
2. **Populate calibration table** — JOIN `signals` against `contracts.settled_yes`, INSERT into the `calibration` hypertable (migration 007). Bucket `model_prob` to nearest 0.1.
3. **Compute rolling metrics** — Per (strategy, station, hour, month): 30-day rolling Brier score, predicted-vs-actual per bucket, avg slippage (`fill_price - market_price_at_order`). Store in a new `calibration_metrics` table.
4. **Update station weights** — Compare current `station_calibration` weights against best recent `backtest_runs` results. Only update if sweep Brier beats current by >0.005.
5. **HRRR skill recalc** — Call `compute_hrrr_skill_scores()` from `physics.py`.
6. **Drift detection** — Compare 7-day vs 30-day Brier. If 7d exceeds 30d by >0.03, send Discord alert via `DISCORD_WEBHOOK_URL` env var (use `aiohttp`).

Follow the same patterns as `python/collector/daemon.py` and `python/evaluator/daemon.py` — structlog logging, asyncpg pool, graceful shutdown.

## Step 3: New migration (019)

Create `migrations/019_calibration_metrics.sql` with the `calibration_metrics` table as specified in the build plan. Use `IF NOT EXISTS` and `ON CONFLICT` patterns consistent with existing migrations.

## Step 4: Evaluator hot-reload (7.6)

In `python/evaluator/daemon.py`, add a 15-minute refresh cycle that reloads `sigma_table`, `climo_table`, and `station_calibration` from the DB without restarting. Track `_last_cal_refresh` as a monotonic timestamp.

## Step 5: Use book-walking fill estimation (7.2b)

In `rust/src/crypto_fv.rs`, the `estimate_fill_price()` function (~line 159) does `mid ± spread/2`. Replace it with a call to `Orderbook::estimated_fill_price(side, size)` from `rust/src/kalshi/orderbook.rs` (line 146), which walks the full book for true VWAP. This requires passing the orderbook reference and anticipated order size into the fair value computation. Fall back to `mid ± spread/2` if the orderbook is unavailable.

## Step 6: Confidence-scaled sizing (7.4)

In `rust/src/order_manager.rs`, modify `compute_order_size()` to accept a `confidence: f64` parameter and scale the Kelly fraction by `confidence.clamp(0.3, 1.0)`. This requires adding `confidence` to the `Signal` struct in `rust/src/types.rs` and populating it from both crypto (`crypto_fv.rs`) and weather (NATS signal) paths.

## Step 7: Exploit unused market data (7.3a-b)

### 7.3a — Stop discarding Kalshi price data

In `rust/src/orderbook_feed.rs`, the `TickerUpdate` handler drops `last_price` and `last_trade_count`. Store them in a per-ticker rolling window (last 60 seconds) for momentum calculation. Add a `kalshi_momentum()` method that returns the slope of mid-price changes. Feed this into `compute_microstructure_adj()` in `crypto_evaluator.rs` as a new component, clamped to ±0.02.

### 7.3b — Use TradeTape VWAP

`TradeTape.vwap()` in `rust/src/kalshi/trade_tape.rs` (line 82) is implemented and tested but never called. Add a VWAP-vs-mid signal to `compute_microstructure_adj()`: `vwap_signal = ((vwap - mid) / spread).clamp(-1.0, 1.0) * 0.02`.

## Step 8: Docker compose + tests

Add `calibrator` service to `docker/docker-compose.yml` (same pattern as `collector` — depends on postgres healthy + migrate completed, needs `DATABASE_URL` and `DISCORD_WEBHOOK_URL`).

Add tests:
- `python/tests/test_calibrator.py` — test outcome settlement logic, calibration table population, drift detection threshold
- Rust tests for any new functions (momentum, VWAP signal, confidence sizing)

## Step 9: Verify

Run `just test` and `just test-python` — all tests must pass. Check that:
- `cargo test` passes (all existing + new tests)
- `python -m pytest python/tests/ -v` passes (all existing + new tests)
- The calibrator daemon starts without errors: `python -m calibrator.daemon`

## Constraints

- Do NOT break existing tests or APIs
- Use `IF NOT EXISTS` / `ON CONFLICT` in all SQL
- Follow existing code patterns (structlog in Python, tracing in Rust)
- Keep backward compat: new `Signal.confidence` field should default to `0.5` so existing NATS messages without it still work
- Do not modify any table schemas from migrations 001-018 — only add new migration 019
