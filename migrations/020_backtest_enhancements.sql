-- Phase 8.0: Backtest enhancements — adds station column and advanced metrics.
-- Fixes calibrator Job 4 which references br.station (previously missing).

ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS station TEXT;

CREATE INDEX IF NOT EXISTS idx_backtest_runs_station
    ON backtest_runs (station, brier_score) WHERE station IS NOT NULL;
