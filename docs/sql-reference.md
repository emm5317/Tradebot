# SQL Reference — Tradebot

## Connection Details

| Field | Value |
|-------|-------|
| Host | `localhost` |
| Port | `15432` |
| Database | `tradebot` |
| Username | `tradebot` |
| Password | `tradebot_dev` |

Quick access: `just db-shell`

---

## 1. Data Collection Health

### Freshness check — latest row per source

```sql
SELECT 'asos'              AS source, MAX(observed_at)  AS latest FROM observations       WHERE source = 'asos'
UNION ALL
SELECT 'binance'           AS source, MAX(observed_at)  AS latest FROM observations       WHERE source = 'binance'
UNION ALL
SELECT 'crypto_ticks'      AS source, MAX(observed_at)  AS latest FROM crypto_ticks
UNION ALL
SELECT 'metar'             AS source, MAX(observed_at)  AS latest FROM metar_observations
UNION ALL
SELECT 'hrrr'              AS source, MAX(forecast_time) AS latest FROM hrrr_forecasts
UNION ALL
SELECT 'market_snapshots'  AS source, MAX(captured_at)  AS latest FROM market_snapshots
UNION ALL
SELECT 'settlement_summary' AS source, MAX(created_at)  AS latest FROM daily_settlement_summary
ORDER BY source;
```

### Volume check — rows collected today

```sql
SELECT 'asos'             AS source, COUNT(*) AS rows_today FROM observations      WHERE source = 'asos'    AND observed_at::date = CURRENT_DATE
UNION ALL
SELECT 'binance'          AS source, COUNT(*) AS rows_today FROM observations      WHERE source = 'binance' AND observed_at::date = CURRENT_DATE
UNION ALL
SELECT 'crypto_ticks'     AS source, COUNT(*) AS rows_today FROM crypto_ticks      WHERE observed_at::date = CURRENT_DATE
UNION ALL
SELECT 'metar'            AS source, COUNT(*) AS rows_today FROM metar_observations WHERE observed_at::date = CURRENT_DATE
UNION ALL
SELECT 'hrrr'             AS source, COUNT(*) AS rows_today FROM hrrr_forecasts    WHERE run_time::date = CURRENT_DATE
UNION ALL
SELECT 'market_snapshots' AS source, COUNT(*) AS rows_today FROM market_snapshots  WHERE captured_at::date = CURRENT_DATE;
```

**Expected daily counts (24hr uptime):**

| Source | ~Rows/day | Interval |
|--------|-----------|----------|
| asos | 7,200 (5 stations × 1440 min) | 60s |
| binance | 1,440 | 60s |
| crypto_ticks | 1,440 | 60s |
| metar | 600 (5 stations, deduped) | 60s |
| hrrr | 1,440 (5 stations × 288 intervals) | 5min |
| market_snapshots | varies | 60s, only near settlement |

### Per-station coverage (last hour)

```sql
SELECT station, COUNT(*) AS obs_count, MIN(observed_at) AS earliest, MAX(observed_at) AS latest
FROM observations
WHERE source = 'asos' AND observed_at > now() - interval '1 hour'
GROUP BY station
ORDER BY station;
```

All 5 stations (KORD, KJFK, KDEN, KLAX, KIAH) should appear with ~60 rows each.

---

## 2. Gap Detection

### ASOS gaps > 5 minutes

```sql
WITH lagged AS (
    SELECT station, observed_at,
           LAG(observed_at) OVER (PARTITION BY station ORDER BY observed_at) AS prev
    FROM observations
    WHERE source = 'asos' AND observed_at > now() - interval '24 hours'
)
SELECT station, prev AS gap_start, observed_at AS gap_end,
       EXTRACT(EPOCH FROM observed_at - prev) / 60 AS gap_minutes
FROM lagged
WHERE observed_at - prev > interval '5 minutes'
ORDER BY gap_minutes DESC
LIMIT 20;
```

### BTC feed gaps > 3 minutes

```sql
WITH lagged AS (
    SELECT observed_at,
           LAG(observed_at) OVER (ORDER BY observed_at) AS prev
    FROM observations
    WHERE source = 'binance' AND observed_at > now() - interval '24 hours'
)
SELECT prev AS gap_start, observed_at AS gap_end,
       EXTRACT(EPOCH FROM observed_at - prev) / 60 AS gap_minutes
FROM lagged
WHERE observed_at - prev > interval '3 minutes'
ORDER BY gap_minutes DESC
LIMIT 10;
```

### METAR gaps > 2 hours per station

```sql
WITH lagged AS (
    SELECT station, observed_at,
           LAG(observed_at) OVER (PARTITION BY station ORDER BY observed_at) AS prev
    FROM metar_observations
    WHERE observed_at > now() - interval '24 hours'
)
SELECT station, prev AS gap_start, observed_at AS gap_end,
       EXTRACT(EPOCH FROM observed_at - prev) / 60 AS gap_minutes
FROM lagged
WHERE observed_at - prev > interval '2 hours'
ORDER BY gap_minutes DESC
LIMIT 10;
```

---

## 3. Settlement & Contract Queries

### Daily settlement summary

```sql
SELECT station, obs_date, final_max_f, final_min_f, metar_max_f, metar_min_f,
       obs_count, contracts_settled
FROM daily_settlement_summary
ORDER BY obs_date DESC, station
LIMIT 20;
```

### Recently settled contracts

```sql
SELECT ticker, category, city, station, threshold,
       settlement_time, settled_yes, close_price
FROM contracts
WHERE settled_yes IS NOT NULL
ORDER BY settlement_time DESC
LIMIT 20;
```

### Active contracts (not yet settled)

```sql
SELECT ticker, category, city, station, threshold,
       settlement_time, status
FROM contracts
WHERE status = 'active'
ORDER BY settlement_time ASC
LIMIT 20;
```

### Contracts settling in the next hour

```sql
SELECT ticker, category, station, threshold, settlement_time,
       EXTRACT(EPOCH FROM settlement_time - now()) / 60 AS minutes_remaining
FROM contracts
WHERE status = 'active'
  AND settlement_time > now()
  AND settlement_time < now() + interval '1 hour'
ORDER BY settlement_time;
```

---

## 4. Signal & Order Analysis

### Recent signals

```sql
SELECT ticker, signal_type, direction, model_prob, market_price,
       edge, kelly_fraction, minutes_remaining, acted_on,
       rejection_reason, created_at
FROM signals
ORDER BY created_at DESC
LIMIT 30;
```

### Signal rejection breakdown

```sql
SELECT rejection_reason, COUNT(*) AS cnt
FROM signals
WHERE rejection_reason IS NOT NULL
  AND created_at > now() - interval '7 days'
GROUP BY rejection_reason
ORDER BY cnt DESC;
```

### Recent orders with outcomes

```sql
SELECT ticker, direction, size_cents, limit_price, fill_price,
       status, outcome, pnl_cents, order_state, signal_type, created_at
FROM orders
ORDER BY created_at DESC
LIMIT 30;
```

### Win/loss by strategy (last 7 days)

```sql
SELECT signal_type,
       COUNT(*) FILTER (WHERE outcome = 'win')  AS wins,
       COUNT(*) FILTER (WHERE outcome = 'loss') AS losses,
       COALESCE(SUM(pnl_cents), 0)              AS total_pnl_cents,
       ROUND(AVG(pnl_cents)::numeric, 1)        AS avg_pnl_cents
FROM orders
WHERE outcome IN ('win', 'loss')
  AND created_at > now() - interval '7 days'
GROUP BY signal_type;
```

---

## 5. Model Components & Calibration

### Model component breakdown for recent signals

```sql
SELECT ticker, signal_type, model_prob, market_price, edge,
       model_components->>'physics' AS physics,
       model_components->>'hrrr'    AS hrrr,
       model_components->>'trend'   AS trend,
       model_components->>'climo'   AS climo,
       model_components->>'rounding' AS rounding,
       created_at
FROM signals
WHERE model_components IS NOT NULL
ORDER BY created_at DESC
LIMIT 20;
```

### Station calibration parameters

```sql
SELECT station, month, hour, sigma_10min, hrrr_bias_f, hrrr_skill,
       weight_physics, weight_hrrr, weight_trend, weight_climo, sample_size
FROM station_calibration
ORDER BY station, month, hour;
```

### Calibration accuracy by probability bucket

```sql
SELECT prob_bucket,
       COUNT(*) AS n,
       ROUND(AVG(model_prob)::numeric, 3)    AS avg_predicted,
       ROUND(AVG(CASE WHEN actual_outcome THEN 1.0 ELSE 0.0 END)::numeric, 3) AS actual_rate
FROM calibration
WHERE settled_at > now() - interval '30 days'
GROUP BY prob_bucket
ORDER BY prob_bucket;
```

---

## 6. Backtesting & Optimization

### Top backtest runs by Brier score

```sql
SELECT run_id, signal_type, start_date, end_date,
       brier_score, accuracy, simulated_pnl_cents,
       total_signals, win_count, loss_count,
       params, description
FROM backtest_runs
WHERE brier_score IS NOT NULL AND brier_score > 0
ORDER BY brier_score ASC
LIMIT 20;
```

### Walk-forward results (out-of-sample)

```sql
SELECT run_id, signal_type,
       train_start, train_end,
       validation_start, validation_end,
       brier_score, accuracy, simulated_pnl_cents,
       params
FROM backtest_runs
WHERE train_start IS NOT NULL
ORDER BY validation_start DESC
LIMIT 20;
```

### Compare parameter configurations

```sql
SELECT params->>'sigma_scale'    AS sigma,
       params->>'weight_physics' AS w_phys,
       params->>'weight_hrrr'    AS w_hrrr,
       params->>'min_edge'       AS min_edge,
       ROUND(AVG(brier_score)::numeric, 4) AS avg_brier,
       ROUND(AVG(accuracy)::numeric, 3)    AS avg_acc,
       SUM(simulated_pnl_cents)            AS total_pnl,
       COUNT(*)                            AS runs
FROM backtest_runs
WHERE signal_type = 'weather' AND brier_score > 0
GROUP BY params->>'sigma_scale', params->>'weight_physics',
         params->>'weight_hrrr', params->>'min_edge'
ORDER BY avg_brier ASC
LIMIT 20;
```

---

## 7. Strategy Performance

### Daily P&L by strategy

```sql
SELECT strategy, date, signals_generated, signals_executed,
       win_count, loss_count, realized_pnl_cents,
       avg_edge, brier_score
FROM strategy_performance
ORDER BY date DESC
LIMIT 30;
```

### Cumulative P&L over time

```sql
SELECT strategy, date, realized_pnl_cents,
       SUM(realized_pnl_cents) OVER (PARTITION BY strategy ORDER BY date) AS cumulative_pnl
FROM strategy_performance
ORDER BY date ASC;
```

### Dead letters (system errors)

```sql
SELECT subject, error, source, created_at
FROM dead_letters
ORDER BY created_at DESC
LIMIT 20;
```

### Reconciliation issues

```sql
SELECT discrepancy, ticker, exchange_qty, local_qty, action_taken, created_at
FROM reconciliation_log
ORDER BY created_at DESC
LIMIT 20;
```

---

## 8. Raw Data Exploration

### Current temperature by station

```sql
SELECT DISTINCT ON (station)
    station, temperature_f, wind_speed_kts, observed_at
FROM observations
WHERE source = 'asos'
ORDER BY station, observed_at DESC;
```

### Latest METAR per station

```sql
SELECT DISTINCT ON (station)
    station, temp_c, dewpoint_c, wind_speed_kts, wx_string,
    max_temp_6hr_c, min_temp_6hr_c, observed_at
FROM metar_observations
ORDER BY station, observed_at DESC;
```

### Latest BTC spot + volatility

```sql
SELECT observed_at, btc_spot, btc_vol_30m
FROM observations
WHERE source = 'binance'
ORDER BY observed_at DESC
LIMIT 1;
```

### HRRR forecast for a station (next 12 hours)

```sql
SELECT station, forecast_time, temp_2m_f, wind_10m_kts, run_time
FROM hrrr_forecasts
WHERE station = 'KORD'
  AND forecast_time > now()
  AND forecast_time < now() + interval '12 hours'
ORDER BY forecast_time;
```

### Market snapshot history for a ticker

```sql
SELECT ticker, yes_price, no_price, spread,
       minutes_to_settlement, captured_at
FROM market_snapshots
WHERE ticker = 'KXTEMP-26MAR08-KORD-B50'  -- replace with actual ticker
ORDER BY captured_at;
```

---

## 9. Table Sizes & Maintenance

### Row counts per table

```sql
SELECT relname AS table_name,
       n_live_tup AS row_count
FROM pg_stat_user_tables
ORDER BY n_live_tup DESC;
```

### Disk usage per table

```sql
SELECT tablename,
       pg_size_pretty(pg_total_relation_size(schemaname || '.' || tablename)) AS total_size
FROM pg_tables
WHERE schemaname = 'public'
ORDER BY pg_total_relation_size(schemaname || '.' || tablename) DESC;
```

### TimescaleDB chunk info (for hypertables)

```sql
SELECT hypertable_name, chunk_name,
       pg_size_pretty(total_bytes) AS size,
       range_start, range_end
FROM timescaledb_information.chunks
ORDER BY range_end DESC
LIMIT 20;
```

---

## Quick Reference — justfile commands

| Command | Purpose |
|---------|---------|
| `just db-shell` | Open psql in Docker |
| `just db-up` | Start infra + migrate |
| `just collector` | Start data collection |
| `just backtest 2026-01-01 2026-03-01` | Run backtest |
| `just sweep 2026-01-01 2026-03-01` | Parameter grid search |
| `just walk-forward 2026-01-01 2026-03-01 14` | Walk-forward optimization |
| `just leaderboard` | Best backtest runs |
| `just settlement-summary` | Aggregate yesterday's settlements |
| `just settlement-backfill 30` | Backfill last 30 days |
| `just health` | Dashboard health check |
